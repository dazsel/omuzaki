//! The [`Connectors`] service (D1, D4, D6): what any crate's op calls through `ctx.service::<dyn Connectors>()`
//! without depending on the host. Implemented by `omuzaki-connect`; cold.

use omuzaki_ai_api::{Progress, Requester};
use omuzaki_ops_api::Actor;
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

use crate::candidate::Candidate;
use crate::connector::RunEnv;
use crate::estimate::{Preview, Status, Widening};
use crate::id::{CandidateId, Capability, ConnectorId, RunId};
use crate::manifest::ConnectorManifest;
use crate::output::{ConnectError, Output, Usage, usd};
use crate::request::{Request, RequestProblem};
use crate::tray::InputRights;

/// What a call is, as [`Connectors::run`] takes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// The connector: the plan's choice, which a job never makes again (D4); `None` lets the host choose for a
    /// preview-less caller.
    pub connector: Option<ConnectorId>,
    /// What to make.
    pub request: Request,
    /// The most it may bill, micro-dollars: the op's `max_usd_micros`, at most the job's reservation.
    pub max_usd_micros: u64,
    /// What the person was shown; a dearer estimate now is [`RunError::PreviewChanged`].
    pub expect_usd_micros: Option<u64>,
    /// The run: the job's id. Seeds and candidate ids derive from it.
    pub run: RunId,
    /// Who asked, as the tray records it.
    pub requested_by: Actor,
    /// Whether a person or an agent asked (the confirmation rule, D5).
    pub requester: Requester,
    /// The candidate this regenerates.
    pub regenerated_from: Option<CandidateId>,
    /// The inputs' effective provenance, resolved from the live document when the job's work started (D4.2).
    pub inputs: InputRights,
    /// The preview the plan showed; the run fails [`RunError::PreviewChanged`] if it would go further.
    pub planned: Option<Preview>,
}

/// What a run gave back. `work` returns it as JSON, the map it used included, for `finish` to land.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    /// The connector that ran.
    pub connector: ConnectorId,
    /// One pending candidate per stored asset, in output order.
    pub candidates: Vec<Candidate>,
    /// Text and JSON answers, which never reach the store or the tray.
    pub answers: Vec<Output>,
    /// What was used.
    pub usage: Usage,
    /// Why the call ended early, if it did; the candidates are the complete outputs.
    pub stopped: Option<ConnectError>,
    /// The input rights the run used, for `finish`'s join (D4.3).
    pub inputs: InputRights,
}

/// Why a preview was refused. Each `Display` says what to do; none holds a secret.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum PreviewError {
    /// No connector has this id.
    #[error("no connector {0}; list the connectors to see the ids")]
    UnknownConnector(String),
    /// No enabled connector offers the capability.
    #[error("no enabled connector makes {0}; enable one on the Connectors page")]
    NoConnector(Capability),
    /// Several enabled connectors offer it; the caller must pick.
    #[error("several connectors make {capability}: {}; name one", list(options))]
    #[non_exhaustive]
    Ambiguous {
        /// The capability.
        capability: Capability,
        /// The connectors that offer it.
        options: Vec<ConnectorId>,
    },
    /// The connector is disabled.
    #[error("connector {0} is disabled; a person enables it on the Connectors page")]
    Disabled(ConnectorId),
    /// Its key is not in the keychain.
    #[error("connector {0} has no key; a person enters it on the Connectors page")]
    NeedsKey(ConnectorId),
    /// The configured plan tier cannot produce shippable output.
    #[error("this needs one of the plans {}{}", allowed.join(", "), have.as_ref().map(|h| format!(", not {h}")).unwrap_or_default())]
    #[non_exhaustive]
    NeedsPlan {
        /// The plans that can.
        allowed: Vec<String>,
        /// The configured plan.
        have: Option<String>,
    },
    /// The request breaks rules.
    #[error("the request is invalid: {}", list(.0))]
    Invalid(Vec<RequestProblem>),
    /// The request names an asset the input rights do not cover.
    #[error("asset {0} is not in the store; import it first")]
    UnknownAsset(ContentHash),
    /// A private test asset would leave the computer.
    #[error("asset {0} is private test material and never leaves this computer; use a local connector")]
    PrivateInput(ContentHash),
    /// A voice clone without the person's consent record.
    #[error("cloning {person}'s voice needs their consent on record; record it first")]
    #[non_exhaustive]
    NeedsConsent {
        /// Whose voice.
        person: String,
    },
    /// The connector cannot do this.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// The connector could not resolve its target or price the request.
    #[error(transparent)]
    Connector(ConnectError),
}

impl PreviewError {
    /// [`PreviewError::Ambiguous`].
    pub fn ambiguous(capability: Capability, options: Vec<ConnectorId>) -> Self {
        Self::Ambiguous { capability, options }
    }

    /// [`PreviewError::NeedsPlan`].
    pub fn needs_plan(allowed: Vec<String>, have: Option<String>) -> Self {
        Self::NeedsPlan { allowed, have }
    }

    /// [`PreviewError::NeedsConsent`].
    pub fn needs_consent(person: impl Into<String>) -> Self {
        Self::NeedsConsent { person: person.into() }
    }
}

/// Why a run failed before any output.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// The preview, rebuilt at run time, refused.
    #[error(transparent)]
    Preview(PreviewError),
    /// The run would go further than the preview its plan showed; nothing was sent.
    #[error("what was approved changed: {what}; preview it again")]
    #[non_exhaustive]
    PreviewChanged {
        /// How.
        what: Widening,
    },
    /// The estimate is above the call's cap; nothing was sent.
    #[error("this would cost {}, above the cap of {}; raise the cap or ask for less", usd(*needed_usd_micros), usd(*cap_usd_micros))]
    #[non_exhaustive]
    CapReached {
        /// The cap, micro-dollars.
        cap_usd_micros: u64,
        /// The estimate, micro-dollars.
        needed_usd_micros: u64,
    },
    /// The connector failed with no output.
    #[error(transparent)]
    Connector(ConnectError),
    /// The store failed.
    #[error("the asset store failed: {0}")]
    Store(String),
}

impl RunError {
    /// [`RunError::PreviewChanged`].
    pub fn preview_changed(what: Widening) -> Self {
        Self::PreviewChanged { what }
    }

    /// [`RunError::CapReached`].
    pub fn cap_reached(cap_usd_micros: u64, needed_usd_micros: u64) -> Self {
        Self::CapReached { cap_usd_micros, needed_usd_micros }
    }
}

fn list<T: std::fmt::Display>(items: &[T]) -> String {
    items.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

/// The connectors service other crates' ops use (`ctx.service::<dyn Connectors>()`), so they reach the host without
/// depending on `omuzaki-connect`. Cold; implemented by the host.
pub trait Connectors: Send + Sync {
    /// Every connector this binary has, sorted by id.
    fn list(&self) -> Vec<ConnectorManifest>;

    /// The manifest of `id`.
    fn describe(&self, id: &ConnectorId) -> Option<ConnectorManifest>;

    /// Whether `id` can run now; `None` for an unknown id.
    fn status(&self, id: &ConnectorId) -> Option<Status>;

    /// What a call would send, where, at what price and under which rights. Pure: calls no connector. `connector`
    /// `None` chooses the one enabled connector for the capability; `inputs` is the caller's
    /// [`tray::input_rights`](crate::tray::input_rights).
    ///
    /// # Errors
    /// A [`PreviewError`] saying what to fix.
    fn preview(
        &self,
        req: &Request,
        connector: Option<&ConnectorId>,
        who: Requester,
        inputs: &InputRights,
    ) -> Result<Preview, PreviewError>;

    /// Runs `call`: the preview again (refusing a widening), the cap, the seed, the connector, then each output
    /// stored with the provenance the host builds and turned into a pending candidate. Blocking, on a job worker.
    ///
    /// # Errors
    /// A [`RunError`] when nothing came back; partial results are an `Ok` with `stopped` set.
    fn run(&self, call: &Call, env: &RunEnv<'_>) -> Result<RunResult, RunError>;

    /// A free reachability check: no billing, no outputs.
    ///
    /// # Errors
    /// What stops the connector from being reached.
    fn ping(&self, id: &ConnectorId, progress: &dyn Progress) -> Result<(), RunError>;
}
