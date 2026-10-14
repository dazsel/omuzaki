//! The executor contract: how the five surfaces (MCP, CLI, scripts, palette, UI) run ops without depending on the
//! executor's implementation (§39 rule 1). `omuzaki-ops` implements [`Runner`]; apps wire it in.

use std::sync::mpsc::Receiver;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocVersion, Patch, Value};
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalRef, PendingApproval, Resolution};
use crate::session::{Ended, SessionChange, SessionId, SessionInfo, SessionUndo};
use crate::{Actor, Cause, Grants, OpError, OpKind, OpMeta, Plan, Scope, Undo};

/// Plan only, or plan and apply.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Validate and return the plan (the patch and any output); change nothing.
    DryRun,
    /// Validate, check permissions, apply, log and emit events.
    Apply,
}

/// One request to run an op.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// The op's registered name, e.g. `face.set_profile`.
    pub op: String,
    /// Its arguments, matching the op's schema.
    pub args: Value,
    /// Dry-run or apply.
    pub mode: Mode,
    /// Who is asking.
    pub actor: Actor,
    /// What the caller may do. For a person this is **trusted transport input**: the transport builds it server-side
    /// (the CLI adds the roots of paths the person typed, ADR 0013) and the executor intersects it with the person's
    /// principal grants. For an agent or plugin it can only narrow: the executor intersects it with the session's
    /// effective grants, so a request can never widen what its session holds.
    pub grants: Grants,
    /// Why (prompt, chat message, rule), for history and the Build Theatre.
    pub cause: Option<Cause>,
    /// The open transaction to add to; `None` makes the op its own one-step transaction.
    pub tx: Option<TxId>,
    /// The document version the caller planned against. When set and the paths this request changes were touched
    /// since, the executor answers `Conflict` with [`ErrorDetail::Changed`](crate::ErrorDetail::Changed) instead of
    /// applying. `None` skips the check.
    #[serde(default)]
    pub base: Option<DocVersion>,
}

/// What running an op produced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Outcome {
    /// The plan: patch, output and cost.
    pub plan: Plan,
    /// The log record, when the op was applied.
    pub applied: Option<OpRecord>,
}

/// A transaction: a group of ops that is one undo step ("make the face more oval").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TxId(pub u64);

/// One applied op in the op log (§6 provenance). Enough to show history, explain changes and replay them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpRecord {
    /// Position in the log, from 1.
    pub seq: u64,
    /// The op's name.
    pub op: String,
    /// Its arguments.
    pub args: Value,
    /// Who ran it.
    pub actor: Actor,
    /// Why.
    pub cause: Option<Cause>,
    /// The transaction it belongs to.
    pub tx: TxId,
    /// When, on the runner's clock.
    pub at: Seconds,
    /// What it changed.
    pub patch: Patch,
    /// The document version after it.
    pub version: DocVersion,
    /// The person the acting session works for. Stamped by the executor from the session registry, never taken from
    /// a request; `None` for persons and the system.
    #[serde(default)]
    pub on_behalf_of: Option<Actor>,
    /// The approval that allowed this op, and its approver. Stamped by the executor when a person approved the run.
    #[serde(default)]
    pub approval: Option<ApprovalRef>,
}

/// Something that happened, streamed to subscribers (UI, Build Theatre, MCP notifications).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// An op was applied.
    OpApplied {
        /// The log record.
        record: OpRecord,
    },
    /// A transaction was committed.
    TxCommitted {
        /// Which.
        tx: TxId,
        /// Its label.
        label: String,
        /// Who committed it (the owner, since only the owner may commit).
        actor: Actor,
    },
    /// A transaction was rolled back; its ops were reverted.
    TxRolledBack {
        /// Which.
        tx: TxId,
        /// Who rolled it back.
        actor: Actor,
        /// The document version after the revert.
        version: DocVersion,
    },
    /// A transaction was undone.
    Undone {
        /// Which.
        tx: TxId,
        /// Its label.
        label: String,
        /// Who did the undo.
        actor: Actor,
        /// Who owns the step.
        owner: Actor,
        /// The document version after the undo.
        version: DocVersion,
    },
    /// An undone transaction was redone.
    Redone {
        /// Which.
        tx: TxId,
        /// Its label.
        label: String,
        /// Who did the redo.
        actor: Actor,
        /// Who owns the step.
        owner: Actor,
        /// The document version after the redo.
        version: DocVersion,
    },
    /// A session opened.
    SessionOpened {
        /// Its snapshot.
        session: SessionInfo,
    },
    /// A session's grants, budget, state or spend changed.
    SessionChanged {
        /// Its snapshot after the change.
        session: SessionInfo,
        /// What changed.
        change: SessionChange,
    },
    /// A session ended.
    SessionEnded {
        /// Its snapshot after the end.
        session: SessionInfo,
        /// What ending it did.
        ended: Ended,
    },
    /// A person undid a whole session.
    SessionUndone {
        /// Which session.
        session: SessionId,
        /// Who.
        by: Actor,
        /// What it did.
        result: SessionUndo,
    },
    /// A request or session is waiting for a person.
    ApprovalRequested {
        /// The pending approval.
        approval: PendingApproval,
    },
    /// An approval was decided or withdrawn.
    ApprovalResolved {
        /// The resolution.
        resolution: Resolution,
    },
}

/// Everything a surface needs to present one op: generated from the registry, never written by hand.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpInfo {
    /// Dotted name.
    pub name: &'static str,
    /// One-line summary.
    pub summary: &'static str,
    /// Edit, query or job.
    pub kind: OpKind,
    /// Required scope.
    pub scope: Scope,
    /// Undo behaviour.
    pub undo: Undo,
    /// The arguments' JSON Schema (inlined).
    pub schema: Schema,
}

impl OpInfo {
    /// Builds the info from static facts and a schema.
    pub fn new(meta: OpMeta, schema: Schema) -> Self {
        Self { name: meta.name, summary: meta.summary, kind: meta.kind, scope: meta.scope, undo: meta.undo, schema }
    }
}

/// Runs ops: the one entry point every surface uses. Methods take `&self` so one runner serves the UI, several MCP
/// sessions and scripts at once; implementations serialise edits internally.
pub trait Runner: Send + Sync {
    /// Every registered op, sorted by name.
    fn ops(&self) -> Vec<OpInfo>;

    /// Runs (or dry-runs) one op.
    ///
    /// Every request needs [`Scope::Read`] (a plan or a conflict message reveals document values), plus the op's own
    /// scope when it's applied. A dry-run of an edit needs only `Read`; a dry-run of a query is gated like the real
    /// thing, because its plan is its answer. An agent or plugin also needs an open session.
    ///
    /// # Errors
    /// Unknown op or bad arguments ([`ErrorCode::Invalid`](crate::ErrorCode::Invalid)), missing scope
    /// (`Denied`, with a hint naming the scope to grant), validation failures from the op, or `Conflict`.
    /// - [`ErrorCode::Waiting`](crate::ErrorCode::Waiting): nothing ran; the request is queued for a person
    ///   (detail `Queued`) or the session is paused. Do not retry: wait for the resolution.
    /// - [`ErrorCode::Limited`](crate::ErrorCode::Limited): the session hit a rate or queue limit; retry after the
    ///   time in the detail.
    fn run(&self, req: Request) -> Result<Outcome, OpError>;

    /// Opens a transaction; ops run with its id join it and become one undo step on commit.
    fn begin(&self, label: &str, actor: &Actor) -> TxId;

    /// Commits an open transaction. Only the actor that began it may commit it.
    ///
    /// # Errors
    /// `NotFound` if `tx` isn't open; `Denied` if `actor` didn't begin it.
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError>;

    /// Reverts every op in an open transaction and closes it. Only the actor that began it may roll it back.
    ///
    /// # Errors
    /// `NotFound` if `tx` isn't open; `Denied` if `actor` didn't begin it; `Conflict` if the document changed in
    /// ways the revert can't apply.
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError>;

    /// Undoes `actor`'s own most recent committed transaction (undo is per actor, as in collaborative editors: an
    /// agent never undoes a person's edit by accident). Returns its label, or `None` if there's nothing to undo.
    ///
    /// If later changes by others make that step impossible to undo, the step is dropped from the actor's history
    /// and a `Conflict` says so (naming the path and who changed it), so history never wedges. Reverting another
    /// actor's transaction is a separate, explicit op.
    ///
    /// # Errors
    /// `Conflict` as above; `Denied` if `grants` can't edit what the step touched.
    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError>;

    /// Redoes `actor`'s most recently undone transaction; returns its label, or `None`. A new commit by the same
    /// actor clears its redo history.
    ///
    /// # Errors
    /// As [`Runner::undo`].
    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError>;

    /// A new event stream. Each subscriber gets every event from now on; a dropped receiver unsubscribes. Events
    /// carry document values, so the stream needs `read`.
    ///
    /// # Errors
    /// `Denied` if `grants` lack [`Scope::Read`].
    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError>;

    /// Reads the document (a query surface for `project.get`-style tools and tests).
    ///
    /// # Errors
    /// `Denied` if `grants` lack [`Scope::Read`].
    fn get(&self, path: &omuzaki_doc_api::DocPath, grants: &Grants) -> Result<Option<Value>, OpError>;

    /// Up to `limit` op-log records with `seq` greater than `after_seq`, oldest first. Records carry arguments and
    /// patches, so the log needs `read`.
    ///
    /// # Errors
    /// `Denied` if `grants` lack [`Scope::Read`].
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError>;
}
