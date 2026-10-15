//! The executor contract: how the five surfaces (MCP, CLI, scripts, palette, UI) run ops without depending on the
//! executor's implementation (§39 rule 1). `omuzaki-ops` implements [`Runner`]; apps wire it in.

use std::sync::mpsc::Receiver;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocVersion, Patch, Value};
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalRef, PendingApproval, Resolution};
use crate::session::{Ended, SessionChange, SessionId, SessionInfo, SessionUndo};
use crate::{Actor, Cause, Effect, Grants, JobId, JobInfo, OpError, OpKind, OpMeta, Plan, Scope, Undo};

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
///
/// `#[non_exhaustive]`: build one with [`Outcome::new`] and [`with_applied`](Self::with_applied). A struct literal
/// outside this crate does not compile:
///
/// ```compile_fail,E0639
/// use omuzaki_ops_api::{Outcome, Plan};
/// let _ = Outcome { plan: Plan::default(), applied: None };
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Outcome {
    /// The plan: patch, output and cost.
    pub plan: Plan,
    /// The log record, when the op was applied.
    pub applied: Option<OpRecord>,
}

impl Outcome {
    /// A dry-run outcome: `plan` and nothing applied.
    pub fn new(plan: Plan) -> Self {
        Self { plan, applied: None }
    }

    /// The same outcome carrying the log `record` of the applied op.
    #[must_use]
    pub fn with_applied(mut self, record: OpRecord) -> Self {
        self.applied = Some(record);
        self
    }
}

/// A transaction: a group of ops that is one undo step ("make the face more oval").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TxId(pub u64);

/// One applied op in the op log (§6 provenance). Enough to show history, explain changes and replay them.
///
/// `#[non_exhaustive]`: build one with [`OpRecord::new`] and the `with_*` methods.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
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
    /// What an [`Undo::Explicit`] op changed outside the document, so the step can be undone and redone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<Effect>,
}

impl OpRecord {
    /// A record of `op` run with `args` by `actor` in transaction `tx` at `at`, which turned the document into
    /// `version` through `patch`. Position `seq` in the log starts from 1. Cause, delegation, approval and effect are
    /// unset; add them with the `with_*` methods.
    #[allow(clippy::too_many_arguments)] // The required facts of a record; everything optional has a `with_*`.
    pub fn new(
        seq: u64,
        op: impl Into<String>,
        args: Value,
        actor: Actor,
        tx: TxId,
        at: Seconds,
        patch: Patch,
        version: DocVersion,
    ) -> Self {
        Self {
            seq,
            op: op.into(),
            args,
            actor,
            cause: None,
            tx,
            at,
            patch,
            version,
            on_behalf_of: None,
            approval: None,
            effect: None,
        }
    }

    /// The same record with its `cause` (a [`Cause`] or an `Option<Cause>`, as a request carries it).
    #[must_use]
    pub fn with_cause(mut self, cause: impl Into<Option<Cause>>) -> Self {
        self.cause = cause.into();
        self
    }

    /// The same record stamped with the person the acting session works for.
    #[must_use]
    pub fn with_on_behalf_of(mut self, person: Actor) -> Self {
        self.on_behalf_of = Some(person);
        self
    }

    /// The same record stamped with the approval that allowed it.
    #[must_use]
    pub fn with_approval(mut self, approval: ApprovalRef) -> Self {
        self.approval = Some(approval);
        self
    }

    /// The same record with the `effect` an explicit-undo op had outside the document.
    #[must_use]
    pub fn with_effect(mut self, effect: Effect) -> Self {
        self.effect = Some(effect);
        self
    }
}

/// Something that happened, streamed to subscribers (UI, Build Theatre, MCP notifications).
///
/// `#[non_exhaustive]`, and so is every variant: a new event or a new field edits only this crate. Build events with
/// the constructors below ([`Event::op_applied`] and so on), match with a wildcard arm and `..` in variant patterns, or
/// forward them through their serialised form. Neither a variant literal nor a `match` without a wildcard arm compiles
/// outside this crate:
///
/// ```compile_fail,E0004
/// use omuzaki_ops_api::Event;
/// fn is_commit(event: &Event) -> bool {
///     match event {
///         Event::TxCommitted { .. } => true,
///     }
/// }
/// ```
///
/// ```compile_fail,E0639
/// use omuzaki_ops_api::{Actor, Event, TxId};
/// let _ = Event::TxCommitted { tx: TxId(1), label: String::new(), actor: Actor::System };
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// An op was applied.
    #[non_exhaustive]
    OpApplied {
        /// The log record.
        record: OpRecord,
    },
    /// A transaction was committed.
    #[non_exhaustive]
    TxCommitted {
        /// Which.
        tx: TxId,
        /// Its label.
        label: String,
        /// Who committed it (the owner, since only the owner may commit).
        actor: Actor,
    },
    /// A transaction was rolled back; its ops were reverted.
    #[non_exhaustive]
    TxRolledBack {
        /// Which.
        tx: TxId,
        /// Who rolled it back.
        actor: Actor,
        /// The document version after the revert.
        version: DocVersion,
    },
    /// A transaction was undone.
    #[non_exhaustive]
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
    #[non_exhaustive]
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
    #[non_exhaustive]
    SessionOpened {
        /// Its snapshot.
        session: SessionInfo,
    },
    /// A session's grants, budget, state or spend changed.
    #[non_exhaustive]
    SessionChanged {
        /// Its snapshot after the change.
        session: SessionInfo,
        /// What changed.
        change: SessionChange,
    },
    /// A session ended.
    #[non_exhaustive]
    SessionEnded {
        /// Its snapshot after the end.
        session: SessionInfo,
        /// What ending it did.
        ended: Ended,
    },
    /// A person undid a whole session.
    #[non_exhaustive]
    SessionUndone {
        /// Which session.
        session: SessionId,
        /// Who.
        by: Actor,
        /// What it did.
        result: SessionUndo,
    },
    /// An agent's edit was refused, or found stale, because of a fence or a newer change. The Theatre's review items
    /// and the status bar read it.
    #[non_exhaustive]
    Clash {
        /// The session that was refused.
        session: SessionId,
        /// Its agent actor.
        actor: Actor,
        /// The op that was refused.
        op: String,
        /// Where.
        path: DocPath,
        /// Why.
        kind: ClashKind,
        /// The other side: the fence's holder, or whoever changed the path.
        other: Option<Actor>,
        /// When, on the runner's clock.
        at: Seconds,
    },
    /// A request or session is waiting for a person.
    #[non_exhaustive]
    ApprovalRequested {
        /// The pending approval.
        approval: PendingApproval,
    },
    /// An approval was decided or withdrawn.
    #[non_exhaustive]
    ApprovalResolved {
        /// The resolution.
        resolution: Resolution,
    },
    /// A job changed state (every change, in order).
    #[non_exhaustive]
    JobChanged {
        /// Its snapshot after the change.
        job: JobInfo,
    },
    /// A job reported progress. Coalesced: at most 10 a second per job.
    #[non_exhaustive]
    JobProgress {
        /// Which job.
        job: JobId,
        /// What it is doing.
        stage: String,
        /// How far along, 0 to 1.
        fraction: f32,
    },
}

impl Event {
    /// An op was applied: `record` is its log record.
    pub fn op_applied(record: OpRecord) -> Self {
        Self::OpApplied { record }
    }

    /// Transaction `tx`, labelled `label`, was committed by `actor`.
    pub fn tx_committed(tx: TxId, label: impl Into<String>, actor: Actor) -> Self {
        Self::TxCommitted { tx, label: label.into(), actor }
    }

    /// Transaction `tx` was rolled back by `actor`, leaving the document at `version`.
    pub fn tx_rolled_back(tx: TxId, actor: Actor, version: DocVersion) -> Self {
        Self::TxRolledBack { tx, actor, version }
    }

    /// Transaction `tx`, labelled `label`, owned by `owner`, was undone by `actor`, leaving the document at `version`.
    pub fn undone(tx: TxId, label: impl Into<String>, actor: Actor, owner: Actor, version: DocVersion) -> Self {
        Self::Undone { tx, label: label.into(), actor, owner, version }
    }

    /// Transaction `tx`, labelled `label`, owned by `owner`, was redone by `actor`, leaving the document at `version`.
    pub fn redone(tx: TxId, label: impl Into<String>, actor: Actor, owner: Actor, version: DocVersion) -> Self {
        Self::Redone { tx, label: label.into(), actor, owner, version }
    }

    /// A session opened; `session` is its snapshot.
    pub fn session_opened(session: SessionInfo) -> Self {
        Self::SessionOpened { session }
    }

    /// A session changed; `session` is its snapshot after `change`.
    pub fn session_changed(session: SessionInfo, change: SessionChange) -> Self {
        Self::SessionChanged { session, change }
    }

    /// A session ended; `session` is its snapshot after the end and `ended` says what ending it did.
    pub fn session_ended(session: SessionInfo, ended: Ended) -> Self {
        Self::SessionEnded { session, ended }
    }

    /// A person (`by`) undid the whole of `session`, with `result`.
    pub fn session_undone(session: SessionId, by: Actor, result: SessionUndo) -> Self {
        Self::SessionUndone { session, by, result }
    }

    /// An edit of `op` at `path` by `actor` in `session` was refused or found stale (`kind`) at `at`; `other` is the
    /// fence's holder or whoever changed the path.
    pub fn clash(
        session: SessionId,
        actor: Actor,
        op: impl Into<String>,
        path: DocPath,
        kind: ClashKind,
        other: Option<Actor>,
        at: Seconds,
    ) -> Self {
        Self::Clash { session, actor, op: op.into(), path, kind, other, at }
    }

    /// A request or session is waiting for a person.
    pub fn approval_requested(approval: PendingApproval) -> Self {
        Self::ApprovalRequested { approval }
    }

    /// An approval was decided or withdrawn.
    pub fn approval_resolved(resolution: Resolution) -> Self {
        Self::ApprovalResolved { resolution }
    }

    /// A job changed state; `job` is its snapshot after the change.
    pub fn job_changed(job: JobInfo) -> Self {
        Self::JobChanged { job }
    }

    /// Job `job` is at `stage`, `fraction` (0 to 1) of the way along.
    pub fn job_progress(job: JobId, stage: impl Into<String>, fraction: f32) -> Self {
        Self::JobProgress { job, stage: stage.into(), fraction }
    }
}

/// Why an agent's edit clashed with someone else's work.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClashKind {
    /// The edit lay outside the agent's fence or inside another session's.
    Fenced,
    /// The path changed since the agent last read it (the stale-write guard).
    Stale,
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
    /// Whether consecutive calls with only number values changed can be coalesced
    /// ([`Op::coalesces`](crate::Op::coalesces)). Omitted from the JSON when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub coalesces: bool,
}

impl OpInfo {
    /// Builds the info from static facts and a schema.
    pub fn new(meta: OpMeta, schema: Schema) -> Self {
        Self {
            name: meta.name,
            summary: meta.summary,
            kind: meta.kind,
            scope: meta.scope,
            undo: meta.undo,
            schema,
            coalesces: false,
        }
    }

    /// The same info with [`coalesces`](Self::coalesces) set.
    #[must_use]
    pub fn with_coalesces(mut self, coalesces: bool) -> Self {
        self.coalesces = coalesces;
        self
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
