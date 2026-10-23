//! Per-session rate limits (design `agent-sessions.md` §8.2): two token buckets per session, refilled from the
//! runner's clock and never from wall time, so tests with a fake clock are exact.
//!
//! **Edits** count applied edits and jobs, `begin`, `commit`, `rollback`, `undo` and `redo`; **runs** count queries
//! and dry-runs. Persons and the system are never limited: the registry only asks for a token for a session.

use omuzaki_ops_api::{ErrorCode, ErrorDetail, Limits, OpError};

/// Which bucket a call draws from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Bucket {
    /// Applied edits and jobs, and the transaction and history calls.
    Edits,
    /// Queries and dry-runs.
    Runs,
}

/// Slack for float rounding: a bucket short of a whole token by less than this holds one, so waiting exactly the
/// time a refusal states always lets the next call through.
const EPSILON: f64 = 1e-9;

/// One token bucket: `tokens` as of `at` (runner seconds).
#[derive(Clone, Debug)]
struct Tokens {
    tokens: f64,
    at: f64,
}

impl Tokens {
    fn full(burst: u32, at: f64) -> Self {
        Self { tokens: f64::from(burst), at }
    }

    /// Takes one token at `at`, refilling `per_minute` a minute up to `burst` first. On refusal, how many seconds until
    /// one token is there.
    fn take(&mut self, per_minute: u32, burst: u32, at: f64) -> Result<(), f64> {
        let rate = f64::from(per_minute) / 60.0;
        // A clock that steps back (it never should) refills nothing rather than draining.
        let elapsed = (at - self.at).max(0.0);
        self.tokens = (self.tokens + elapsed * rate).min(f64::from(burst));
        self.at = self.at.max(at);
        if self.tokens + EPSILON >= 1.0 {
            self.tokens = (self.tokens - 1.0).max(0.0);
            Ok(())
        } else {
            Err((1.0 - self.tokens) / rate)
        }
    }
}

/// A session's two buckets.
#[derive(Clone, Debug)]
pub(crate) struct Buckets {
    edits: Tokens,
    runs: Tokens,
}

impl Buckets {
    /// Full buckets for `limits`, as of `at`.
    pub(crate) fn new(limits: &Limits, at: f64) -> Self {
        Self { edits: Tokens::full(limits.edit_burst, at), runs: Tokens::full(limits.run_burst, at) }
    }

    /// Takes one token from `bucket` at `at`.
    ///
    /// # Errors
    /// `Limited`, detail [`ErrorDetail::RetryAfter`] with the seconds until a token is there and the limit's name
    /// and value. Nothing was taken.
    pub(crate) fn take(&mut self, limits: &Limits, bucket: Bucket, at: f64) -> Result<(), OpError> {
        let (tokens, per_minute, burst, name) = match bucket {
            Bucket::Edits => (&mut self.edits, limits.edits_per_minute, limits.edit_burst, "edits_per_minute"),
            Bucket::Runs => (&mut self.runs, limits.runs_per_minute, limits.run_burst, "runs_per_minute"),
        };
        tokens.take(per_minute, burst, at).map_err(|wait| limited(name, per_minute, wait))
    }
}

/// Refuses a session's limits with a zero in them: a zero rate or burst would refuse every call, a zero
/// `max_pending` every approval.
///
/// # Errors
/// `Invalid`, naming the field.
pub(crate) fn validate(limits: &Limits) -> Result<(), OpError> {
    let fields = [
        ("limits.edits_per_minute", limits.edits_per_minute),
        ("limits.edit_burst", limits.edit_burst),
        ("limits.runs_per_minute", limits.runs_per_minute),
        ("limits.run_burst", limits.run_burst),
        ("limits.max_pending", limits.max_pending),
    ];
    match fields.into_iter().find(|(_, value)| *value == 0) {
        Some((field, _)) => Err(OpError::invalid(field, format!("{field} must be at least 1"))
            .hint("leave the limits at their defaults unless a person chose others")),
        None => Ok(()),
    }
}

/// The refusal for a full queue of approvals: `max_pending` are already waiting.
pub(crate) fn queue_full(max_pending: u32) -> OpError {
    let mut error = OpError::denied(format!(
        "this session already has {max_pending} approvals waiting for a person (max_pending = {max_pending})"
    ))
    .hint("wait for a person to decide the ones waiting, then retry")
    .with_detail(ErrorDetail::RetryAfter { seconds: 1.0, limit: format!("max_pending = {max_pending}") });
    error.code = ErrorCode::Limited;
    error
}

/// `Limited` for the limit `name` (`per_minute` a minute), with the wait rounded up to the next `f32`, so waiting the
/// stated time is always enough.
fn limited(name: &str, per_minute: u32, wait: f64) -> OpError {
    #[allow(clippy::cast_possible_truncation)] // A wait is at most a minute's worth of seconds.
    let mut seconds = wait as f32;
    if f64::from(seconds) < wait {
        seconds = seconds.next_up();
    }
    let limit = format!("{name} = {per_minute}");
    let mut error = OpError::denied(format!("this session reached its rate limit ({limit}); retry in {seconds:.3} s"))
        .hint("slow down: wait the time in the detail, then retry")
        .with_detail(ErrorDetail::RetryAfter { seconds, limit });
    error.code = ErrorCode::Limited;
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_passes_then_the_next_waits_exactly_one_tokens_time() {
        let limits = Limits::default();
        let mut buckets = Buckets::new(&limits, 0.0);
        for _ in 0..limits.edit_burst {
            buckets.take(&limits, Bucket::Edits, 0.0).unwrap();
        }
        let e = buckets.take(&limits, Bucket::Edits, 0.0).unwrap_err();
        let Some(ErrorDetail::RetryAfter { seconds, limit }) = e.detail.as_deref() else { panic!("{e:?}") };
        assert_eq!(limit, "edits_per_minute = 600");
        assert!((f64::from(*seconds) - 0.1).abs() < 1e-6, "{seconds}");
        buckets.take(&limits, Bucket::Edits, f64::from(*seconds)).unwrap();
        assert_eq!(buckets.take(&limits, Bucket::Edits, f64::from(*seconds)).unwrap_err().code, ErrorCode::Limited);
        buckets.take(&limits, Bucket::Runs, 0.0).unwrap();
    }

    #[test]
    fn a_zero_limit_is_named() {
        let e = validate(&Limits { run_burst: 0, ..Limits::default() }).unwrap_err();
        assert_eq!(e.field.as_deref(), Some("limits.run_burst"));
        validate(&Limits::default()).unwrap();
    }
}
