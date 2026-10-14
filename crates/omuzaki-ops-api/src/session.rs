//! Agent sessions: who an agent is, what it may do and what it has spent (design `agent-sessions.md` §3, §6, §8).
//!
//! The executor owns every value here. A transport describes a client in a [`SessionSpec`]; the executor allocates the
//! [`SessionId`], builds the agent [`Actor`], derives the effective grants and stamps the principal onto each log
//! record. Nothing in a [`Request`](crate::Request) can claim any of it.

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::DocPath;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalId, Decision, PendingApproval, Resolution};
use crate::{Actor, Grants, OpError, TxId};

/// Identifies one session. Allocated by the executor and unique within the project's history.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SessionId(pub u64);

/// How the client is connected. Informational and for the Agent panel: it never widens grants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum Transport {
    /// MCP over stdio.
    Stdio,
    /// MCP over HTTP.
    Http {
        /// The token's label, never its secret.
        token: String,
    },
    /// The built-in assistant.
    InApp,
    /// A plugin.
    Plugin,
    /// An automation rule (BR-11).
    Automation,
    /// A replay of a recorded session (TH-12).
    Replay,
}

/// What a transport asks for when it admits a client. The transport fills it from what it has authenticated; the
/// executor decides the rest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSpec {
    /// How the client is connected.
    pub transport: Transport,
    /// The client's name. The executor sanitises it before it goes into the agent actor's id.
    pub client: String,
    /// The model, when the client says (the in-app host always knows).
    pub model: Option<String>,
    /// The person the session works for, from what the transport authenticated (the token's issuer, the studio's
    /// person). Must be an [`Actor::Person`]: the executor refuses anything else.
    pub on_behalf_of: Actor,
    /// The ceiling the session starts with (before the intersection with the principal's grants).
    pub grants: Grants,
    /// Its spending caps.
    pub budget: Budget,
    /// Its rate limits.
    pub limits: Limits,
    /// The spawning session, for sub-agents (AG-03).
    pub parent: Option<SessionId>,
    /// The role file's name, for sub-agents.
    pub role: Option<String>,
}

/// Why a session is paused.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// A person paused (or stopped) it.
    Person,
    /// Its budget ran out.
    Budget,
    /// A person is undoing the session's work.
    Undo,
    /// Its pause mode asked for a stop after a step (TH-06).
    Step,
    /// A bridge connection waits for a person to pair its credential.
    Pairing,
}

/// Why a session ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum EndReason {
    /// The connection went away (stdio EOF, HTTP `DELETE`).
    Disconnected,
    /// The HTTP idle timeout passed.
    IdleTimeout,
    /// The client closed the session.
    Closed,
    /// A person revoked it.
    Revoked {
        /// Who.
        by: Actor,
        /// The transport also revokes the session's credential, so the agent cannot come back. The UI defaults to
        /// `true`.
        credential: bool,
    },
    /// The project closed.
    ProjectClosed,
}

/// Where a session stands. "Working", "idle" and "waiting for you" are derived from this plus
/// [`SessionInfo::last_active_at`] and [`SessionInfo::pending`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionState {
    /// Running requests.
    Active,
    /// Requests are refused with [`ErrorDetail::Paused`](crate::ErrorDetail::Paused).
    Paused {
        /// Who paused it (a person, or the agent actor itself for a budget pause set by the executor).
        by: Actor,
        /// Why.
        reason: PauseReason,
    },
    /// Over. Kept in the registry for history, the Theatre and per-principal undo.
    Ended {
        /// Why.
        reason: EndReason,
        /// When, on the runner's clock.
        at: Seconds,
    },
}

/// A session's caps (design §8.1). Money defaults to 0: a session that holds `spend` must be given a cap.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Budget {
    /// Money, in integer micro-dollars so sums are exact and deterministic.
    pub usd_micros: u64,
    /// LLM tokens; `None` is no cap.
    pub tokens: Option<u64>,
    /// Active wall time in seconds; `None` is no cap.
    pub seconds: Option<u64>,
}

/// What a session has used, in the same three units as [`Budget`]. Charged by the executor (money, time) and by
/// whoever pays for the model (tokens, through [`Sessions::charge`]).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Spend {
    /// Micro-dollars.
    pub usd_micros: u64,
    /// LLM tokens.
    pub tokens: u64,
    /// Active wall time in seconds (paused time excluded).
    pub seconds: u64,
}

/// Per-session rate limits, enforced by the executor on the runner's clock and never on persons (design §8.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Limits {
    /// Sustained rate of applied edits, jobs, `begin`, `commit`, `rollback`, `undo` and `redo`.
    pub edits_per_minute: u32,
    /// How many of those may come at once.
    pub edit_burst: u32,
    /// Sustained rate of queries and dry-runs.
    pub runs_per_minute: u32,
    /// How many of those may come at once.
    pub run_burst: u32,
    /// Most approvals queued at once.
    pub max_pending: u32,
}

impl Default for Limits {
    /// The accepted defaults: 600 / 100 / 3000 / 300 / 16 (design §8.2).
    fn default() -> Self {
        Self { edits_per_minute: 600, edit_burst: 100, runs_per_minute: 3000, run_burst: 300, max_pending: 16 }
    }
}

/// A session's open transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionTx {
    /// Which.
    pub tx: TxId,
    /// Its label.
    pub label: String,
}

/// One session as the Agent panel, `session_list` and `omuzaki session list` show it (design §3.1). A snapshot built by
/// the executor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    /// Its id.
    pub id: SessionId,
    /// The agent actor the executor built from the sanitised client name and the id. Transports use it and never
    /// make their own.
    pub actor: Actor,
    /// The person the session works for.
    pub on_behalf_of: Actor,
    /// How it is connected.
    pub transport: Transport,
    /// The sanitised client name.
    pub client: String,
    /// The model, when known.
    pub model: Option<String>,
    /// The spawning session, for sub-agents.
    pub parent: Option<SessionId>,
    /// The role file's name, for sub-agents.
    pub role: Option<String>,
    /// The effective grants: the ceiling intersected with the principal's grants.
    pub grants: Grants,
    /// Document paths the session may edit. Empty means the whole project until AG-04 defines fences.
    pub fence: Vec<DocPath>,
    /// Its caps.
    pub budget: Budget,
    /// What it has used.
    pub spent: Spend,
    /// Its rate limits.
    pub limits: Limits,
    /// Where it stands.
    pub state: SessionState,
    /// Its open transaction, if any.
    pub open_tx: Option<SessionTx>,
    /// Its pending approvals.
    pub pending: Vec<ApprovalId>,
    /// Committed transactions so far.
    pub steps: u32,
    /// When it opened, on the runner's clock.
    pub started_at: Seconds,
    /// Its last call, on the runner's clock.
    pub last_active_at: Seconds,
}

/// What changed in a session, for [`Event::SessionChanged`](crate::Event::SessionChanged).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum SessionChange {
    /// Its grants changed.
    Grants {
        /// Who changed them.
        by: Actor,
    },
    /// Its budget changed.
    Budget {
        /// Who changed it.
        by: Actor,
    },
    /// It was paused; the new [`SessionState`] says by whom and why.
    Paused,
    /// It was resumed.
    Resumed {
        /// Who.
        by: Actor,
    },
    /// Its spend moved.
    Spent,
}

/// A transaction that could not be kept when a session ended, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeptTx {
    /// Which.
    pub tx: TxId,
    /// Its label.
    pub label: String,
    /// Why it was kept (or skipped): for an end, the abandon that could not revert; for an undo, the conflict.
    pub error: OpError,
}

/// What ending a session did, atomically (design §3.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ended {
    /// Open transactions that were reverted and dropped.
    pub abandoned: Vec<TxId>,
    /// Open transactions whose revert conflicted, so their changes were kept.
    pub kept: Vec<KeptTx>,
    /// Pending approvals that were withdrawn.
    pub withdrawn: Vec<ApprovalId>,
}

/// What undoing a whole session did (design §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionUndo {
    /// The one step the reverts became in the person's history, if anything was reverted.
    pub step: Option<TxId>,
    /// Committed steps that were reverted, newest first.
    pub reverted: Vec<TxId>,
    /// Steps whose revert conflicted and were skipped.
    pub skipped: Vec<KeptTx>,
    /// Open transactions that were abandoned.
    pub abandoned: Vec<TxId>,
    /// Steps with effects outside the document ([`Undo::Explicit`](crate::Undo::Explicit)) that cannot be undone yet.
    pub outside: Vec<TxId>,
}

/// Where the executor finds a person's grants (design §3.3). The app registers one; phase 1 has a single local person
/// holding [`Grants::person`]. VC-10 roles narrow it later without touching sessions.
pub trait Principals: Send + Sync {
    /// What `person` may do. An actor that is not a person holds nothing.
    fn grants(&self, person: &Actor) -> Grants;
}

/// The session registry and its controls. Every control takes `by`, which must be the session's principal (a person);
/// an agent, including the session itself, is `Denied`. Each call is atomic under the executor's lock and emits an
/// event. The panel and the Build Theatre update from events only.
pub trait Sessions: Send + Sync {
    /// Opens a session for the client `spec` describes.
    ///
    /// # Errors
    /// `Invalid` if `on_behalf_of` is not a person or `parent` is unknown; `Denied` if the ceiling exceeds the
    /// principal's or the parent's grants.
    fn open(&self, spec: SessionSpec) -> Result<SessionInfo, OpError>;

    /// Every session, ended ones included, oldest first.
    fn sessions(&self) -> Vec<SessionInfo>;

    /// One session.
    fn session(&self, id: SessionId) -> Option<SessionInfo>;

    /// The session whose agent actor is `actor`.
    fn session_of(&self, actor: &Actor) -> Option<SessionInfo>;

    /// Pauses a session ([`PauseReason::Person`]); its open transaction is kept.
    ///
    /// # Errors
    /// `NotFound` for an unknown session; `Denied` if `by` is not its principal; `Conflict` if it has ended.
    fn pause(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError>;

    /// Resumes a paused session. One paused for its budget resumes only once the budget allows.
    ///
    /// # Errors
    /// As [`pause`](Self::pause); `Denied` if the session's budget still blocks it.
    fn resume(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError>;

    /// Replaces the session's ceiling, up to the principal's own grants and never by an agent.
    ///
    /// # Errors
    /// As [`pause`](Self::pause); `Denied` if `grants` exceed the principal's.
    fn set_grants(&self, id: SessionId, grants: Grants, by: &Actor) -> Result<SessionInfo, OpError>;

    /// Replaces the session's budget.
    ///
    /// # Errors
    /// As [`pause`](Self::pause).
    fn set_budget(&self, id: SessionId, budget: Budget, by: &Actor) -> Result<SessionInfo, OpError>;

    /// Adds `spend` to the session's counters. Whoever pays for the model calls this (the in-app host); a charge that
    /// crosses a cap pauses the session and queues a budget approval.
    ///
    /// # Errors
    /// `NotFound` for an unknown session; `Conflict` if it has ended.
    fn charge(&self, id: SessionId, spend: Spend) -> Result<SessionInfo, OpError>;

    /// Ends a session: abandons its open transactions, withdraws its approvals, drops its per-session grants.
    ///
    /// # Errors
    /// `NotFound` for an unknown session; `Conflict` if it has already ended.
    fn end(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError>;

    /// Reverts the session's committed steps, newest first, as one step in the person's history (design §6).
    ///
    /// # Errors
    /// `NotFound` for an unknown session; `Denied` if `by` is not its principal.
    fn undo_session(&self, id: SessionId, by: &Actor) -> Result<SessionUndo, OpError>;

    /// The approvals waiting for a person, oldest first.
    fn approvals(&self) -> Vec<PendingApproval>;

    /// Decides one approval, exactly once. On approval the request is re-planned and run against the current
    /// document as the agent with the need satisfied for that run.
    ///
    /// # Errors
    /// `NotFound` if the approval is gone (resolved or withdrawn); `Denied` if `by` is not a person, not the
    /// session's principal, or lacks the needed scope; `Invalid` if `decision` does not fit the need.
    fn resolve(&self, id: ApprovalId, decision: Decision, by: &Actor) -> Result<Resolution, OpError>;
}
