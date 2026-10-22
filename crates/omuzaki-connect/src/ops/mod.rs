//! The `connect.*` ops (D4, D7). Every one reaches the host through `ctx.service::<dyn Connectors>()` (key status
//! through the concrete [`Host`](crate::Host)), so any App that wires the host gets them. No op argument can hold a
//! URL, a command line or a key: endpoints and command lines are a person's settings, keys are entered in the UI.
//!
//! | Op | Kind, scope, undo | Does |
//! |---|---|---|
//! | `connect.list` | query, `read` | every connector: id, name, capabilities, paid, leaves the machine, status |
//! | `connect.describe` | query, `read` | the manifest, with its params |
//! | `connect.key_status` | query, `read` | whether the key is present; never the key |
//! | `connect.preview` | query, `read` | what a call would send, where, at what price, under which rights |
//! | `connect.generate` | job, `net`, auto | the preview as its plan; the run; its candidates land in the tray in one step |
//! | `connect.test` | job, `net`, none | a free reachability check |
//! | `connect.tray_list` | query, `read` | the tray's records |
//! | `connect.accept`, `connect.reject` | edit, `edit:library`, auto | decide a pending candidate |
//! | `connect.regenerate` | job, `net`, auto | re-issue a candidate's request, with a new seed or the same |

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_ai_api::{Progress, Requester};
use omuzaki_connect_api::{ConnectError, ConnectorId, Connectors, Meter, PreviewError, RunError, TrayError, usd};
use omuzaki_ops_api::{Actor, CancelFlag, ErrorCode, JobCtx, OpCtx, OpError};

pub mod call;
pub mod describe;
pub mod generate;
pub mod key_status;
pub mod list;
pub mod preview;
pub mod regenerate;
pub mod test;
pub mod tray;

/// The host, as the service every crate's op reaches.
fn connectors<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn Connectors, OpError> {
    ctx.service::<dyn Connectors>()
}

/// Whether a person or an agent asks. Plugins and automations count as agents: they never send a project's assets off
/// the computer without a person (D5).
fn requester(actor: &Actor) -> Requester {
    match actor {
        Actor::Person { .. } => Requester::Person,
        _ => Requester::Agent,
    }
}

/// `connector` is not one of the host's: `NotFound` listing the ids.
fn unknown(connector: &ConnectorId, host: &dyn Connectors) -> OpError {
    let ids: Vec<String> = host.list().into_iter().map(|m| m.id.to_string()).collect();
    OpError::not_found(format!("no connector {connector}"))
        .hint(format!("the connectors are: {}", if ids.is_empty() { "(none)".to_owned() } else { ids.join(", ") }))
}

/// An error with another code.
fn coded(mut e: OpError, code: ErrorCode) -> OpError {
    e.code = code;
    e
}

const SETTINGS_HINT: &str = "a person sets it up on the Connectors page";

/// What a refused preview means for an op.
fn preview_error(e: PreviewError) -> OpError {
    let message = e.to_string();
    match e {
        PreviewError::UnknownConnector(_) => OpError::not_found(message).hint("connect.list shows the ids"),
        PreviewError::NoConnector(_) | PreviewError::Disabled(_) | PreviewError::NeedsKey(_) => {
            coded(OpError::unsupported(message), ErrorCode::Unavailable).hint(SETTINGS_HINT)
        }
        PreviewError::Ambiguous { .. } => OpError::invalid("connector", message),
        PreviewError::Invalid(problems) => {
            let field = problems.first().map_or_else(|| "request".to_owned(), |p| format!("request/{}", p.field));
            OpError::invalid(field, message)
        }
        PreviewError::UnknownAsset(_) => OpError::not_found(message),
        PreviewError::PrivateInput(_) => OpError::denied(message),
        PreviewError::NeedsConsent { .. } => OpError::denied(message).hint("record the person's consent first"),
        PreviewError::Connector(c) => connect_error(&c),
        _ => OpError::unsupported(message),
    }
}

/// What a connector's error means for an op.
fn connect_error(e: &ConnectError) -> OpError {
    let message = e.to_string();
    match e {
        ConnectError::Unavailable { .. } | ConnectError::Timeout { .. } => {
            coded(OpError::unsupported(message), ErrorCode::Unavailable)
        }
        ConnectError::Auth { .. } => OpError::denied(message).hint(SETTINGS_HINT),
        ConnectError::RateLimited { .. } | ConnectError::CapReached { .. } => {
            coded(OpError::unsupported(message), ErrorCode::Limited)
        }
        ConnectError::Cancelled => coded(OpError::unsupported(message), ErrorCode::Cancelled),
        _ => OpError::unsupported(message),
    }
}

/// What a failed run means for an op.
fn run_error(e: RunError) -> OpError {
    let message = e.to_string();
    match e {
        RunError::Preview(p) => preview_error(p),
        RunError::PreviewChanged { .. } => OpError::conflict(message).hint("preview it again, then run that"),
        RunError::CapReached { .. } => coded(OpError::unsupported(message), ErrorCode::Limited),
        RunError::Connector(c) => connect_error(&c),
        _ => OpError::internal(message),
    }
}

/// What a tray failure means for an op.
#[allow(clippy::needless_pass_by_value, reason = "it is the map_err of every tray call")]
fn tray_error(e: TrayError) -> OpError {
    let message = e.to_string();
    match e {
        TrayError::Missing(_) | TrayError::UnknownAsset(_) => OpError::not_found(message),
        TrayError::AlreadyDecided(_) | TrayError::Duplicate(_) => OpError::conflict(message),
        TrayError::Undecided(_) => OpError::invalid("state", message),
        _ => OpError::internal(message),
    }
}

/// Micro-dollars as the dollars a `CostHint` holds. Through the decimal text, so the executor's reservation (which
/// reads the shortest decimal form) gets back the same micro-dollars for any amount below about $10.
#[allow(clippy::cast_possible_truncation, reason = "a dollar amount fits an f32 hint; the exact value stays in micros")]
fn dollars(micros: u64) -> f32 {
    usd(micros).trim_start_matches('$').parse::<f64>().unwrap_or(f64::MAX) as f32
}

/// A job's progress and cancel check as the connector contract's [`Progress`].
struct JobProgress<'a>(&'a JobCtx<'a>);

impl Progress for JobProgress<'_> {
    fn stage(&self, name: &str, fraction: f32) {
        self.0.progress(name, fraction);
    }

    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// A job's owned cancel flag as the HTTP contract's cancel handle (for streams that outlive a borrow).
struct JobCancel(CancelFlag);

impl omuzaki_http_api::Cancel for JobCancel {
    fn is_cancelled(&self) -> bool {
        omuzaki_ops_api::Cancel::is_cancelled(&self.0)
    }
}

impl JobCancel {
    fn of(job: &JobCtx<'_>) -> Arc<dyn omuzaki_http_api::Cancel> {
        Arc::new(Self(job.cancel_flag()))
    }
}

/// Every charge goes to the job's money the moment it is billed (D4.6): a store failure or a cancel afterwards
/// cannot lose it. The job refuses past its reservation, which is the call's cap.
struct JobMeter<'a> {
    job: &'a JobCtx<'a>,
    total: AtomicU64,
}

impl<'a> JobMeter<'a> {
    fn new(job: &'a JobCtx<'a>) -> Self {
        Self { job, total: AtomicU64::new(0) }
    }
}

impl Meter for JobMeter<'_> {
    fn charge(&self, usd_micros: u64) -> Result<(), ConnectError> {
        let before = self.total.load(Ordering::Relaxed);
        match self.job.spend(usd_micros) {
            Ok(()) => {
                self.total.fetch_add(usd_micros, Ordering::Relaxed);
                Ok(())
            }
            Err(_) => Err(ConnectError::cap_reached(self.job.reserved_usd_micros(), before.saturating_add(usd_micros))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollars_round_trip_through_the_reservation_rule() {
        // The executor's rule: the hint's shortest decimal text, times a million, rounded up.
        for micros in [0u32, 1, 40_000, 80_000, 100_000, 123_457, 9_999_999] {
            let back = dollars(u64::from(micros)).to_string().parse::<f64>().unwrap() * 1e6;
            assert_eq!(back.ceil(), f64::from(micros), "{micros}");
        }
    }
}
