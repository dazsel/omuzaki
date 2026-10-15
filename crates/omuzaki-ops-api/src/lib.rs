//! Contract for the operation layer (spec v3 §6): every state change in Omuzaki is an **op**.
//!
//! An op is a typed, serialisable struct with a JSON Schema. It never mutates anything itself: [`Op::plan`] reads
//! the document and returns a [`Plan`] (a [`Patch`] plus, for queries, an output). The executor (`omuzaki-ops`) checks
//! the actor's [`Grants`], applies the patch in a transaction, logs it and emits events. That split gives the §6
//! properties by construction:
//! - **dry-run** = call `plan` and show the patch
//! - **undo** = apply `patch.inverse()` ([`Undo::Auto`])
//! - **validation** happens in `plan`, before anything changes
//! - **one registry, five surfaces**: MCP tools, CLI, scripts, the command palette and docs are all generated from
//!   [`registrations`]
//!
//! Ops **register themselves** with [`register_op!`] (§39 rule 2): adding an op is adding one file, and nothing edits
//! a central list.
#![deny(missing_docs)]

mod actor;
mod approval;
pub mod checks;
mod ctx;
mod error;
mod fence;
mod file_access;
mod jobs;
mod nested;
mod registry;
mod runner;
mod scope;
mod services;
mod session;

pub use actor::{Actor, Cause, CauseKind, Grants};
pub use approval::{ApprovalId, ApprovalRef, ApprovalRequest, Decision, Need, PendingApproval, Resolution, Resolved};
pub use ctx::OpCtx;
pub use error::{ErrorCode, ErrorDetail, OpError, RevertConflict};
pub use fence::{fence_covers, fence_overlap, fence_within, normalize_fence};
pub use file_access::{
    CanonicalPath, DirEntryInfo, EntryKind, FsMode, ProjectDir, ProjectDirError, Replacement, WriteMode,
};
pub use jobs::{
    Cancel, CancelFlag, CancelReason, JobCalls, JobCtx, JobDoc, JobId, JobInfo, JobShape, JobSignal, JobState, Jobs,
    Lane, NeverCancel,
};
pub use registry::{OpRegistration, find, registrations};
pub use runner::{ClashKind, Event, Mode, OpInfo, OpRecord, Outcome, Request, Runner, TxId};
pub use scope::Scope;
pub use services::{ConnectionService, Service, Services, ServicesBuilder};
pub use session::{
    Budget, EndReason, Ended, KeptTx, Limits, PauseReason, Principals, SessionChange, SessionId, SessionInfo,
    SessionSpec, SessionState, SessionTx, SessionUndo, Sessions, Spend, StopStep, Stopped, Transport,
};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

use omuzaki_doc_api::{Patch, Value};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// What kind of op this is; decides approval and undo behaviour.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    /// Changes the document. Undoable.
    Edit,
    /// Read-only (`project.get`, `measure.*`). Never needs approval, never changes the document.
    Query,
    /// Long-running (renders, generation, export): runs in the job manager with progress, cancellation and caps.
    Job,
}

/// How an op is undone.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Undo {
    /// The inverse of its patch undoes it completely (most ops).
    Auto,
    /// It has effects outside the document (a file written, a stream started); it undoes those itself.
    Explicit,
    /// Nothing to undo (queries).
    None,
}

/// Static facts about an op type, shared by every surface that exposes it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct OpMeta {
    /// Dotted name, `<area>.<verb>[_<noun>]`, e.g. `face.set_profile`. It's the MCP tool name and the CLI path.
    pub name: &'static str,
    /// One line for humans and agents: what it does, in the imperative.
    pub summary: &'static str,
    /// Edit, query or job.
    pub kind: OpKind,
    /// The scope an actor needs to run it.
    pub scope: Scope,
    /// How it's undone.
    pub undo: Undo,
}

/// Rough cost, shown before anything expensive runs (§6 "cost hints").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CostHint {
    /// Expected wall time, seconds.
    pub seconds: Option<f32>,
    /// Peak GPU memory, MiB.
    pub gpu_mib: Option<u32>,
    /// Money for paid connectors, US dollars.
    pub usd: Option<f32>,
}

/// A call to a registered op, stored so an effect outside the document can be undone and redone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpCall {
    /// The op's registered name.
    pub op: String,
    /// Its arguments.
    pub args: Value,
}

/// What an [`Undo::Explicit`] op changed outside the document, as a pair of inverse calls. The forward op writes
/// both with exactly what it did; inverse ops are fast, local and idempotent, and plan an empty patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Effect {
    /// What happened, for history and the undo prompt.
    pub summary: String,
    /// Reverses the effect.
    pub undo: OpCall,
    /// Puts the effect back as it was.
    pub redo: OpCall,
}

/// The result of planning an op: what would change, what it returns, and what it costs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    /// The document changes. Empty for queries.
    pub patch: Patch,
    /// The answer for queries, or a summary for edits (shown in history and to agents).
    pub output: Option<Value>,
    /// Cost estimate, when it isn't trivial.
    pub cost: Option<CostHint>,
    /// What an [`Undo::Explicit`] op changed outside the document. Absent from the JSON when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<Effect>,
}

impl Plan {
    /// A plan that changes the document.
    pub fn edit(patch: Patch) -> Self {
        Self { patch, ..Self::default() }
    }

    /// A plan that only answers.
    pub fn answer(output: Value) -> Self {
        Self { output: Some(output), ..Self::default() }
    }
}

/// A typed operation. Implement it (or, from phase 1, derive it with `#[derive(Op)]`) and register it with
/// [`register_op!`].
pub trait Op: Serialize + DeserializeOwned + JsonSchema + Send + Sync + 'static {
    /// The op's static facts.
    fn meta() -> OpMeta;

    /// Validates the arguments against the document and computes the change, without applying anything.
    ///
    /// # Errors
    /// An [`OpError`] that tells the caller exactly what to fix: which field, why, and a hint.
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError>;

    /// For a job ([`OpKind::Job`]): the long-running part. Runs on a job worker **without** the document lock and
    /// returns a JSON result; it reads the document only through [`JobCtx::read`]. The default refuses: the op is
    /// not a job.
    ///
    /// # Errors
    /// Whatever the work fails with, or `Cancelled` from [`JobCtx::checkpoint`].
    fn work(&self, _job: &JobCtx<'_>) -> Result<Value, OpError> {
        Err(jobs::not_a_job(Self::meta().name))
    }

    /// For a job: turns `work`'s result into the [`Plan`] the executor applies, under the lock against the document
    /// as it is then. The default answers with the result and changes nothing.
    ///
    /// # Errors
    /// An [`OpError`], as [`plan`](Self::plan).
    fn finish(&self, _ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        Ok(Plan::answer(result))
    }

    /// Whether a job is [`Plain`](JobShape::Plain) (the default) or a [`Driver`](JobShape::Driver).
    fn job_shape() -> JobShape
    where
        Self: Sized,
    {
        JobShape::Plain
    }

    /// Whether consecutive calls can be coalesced. "Applying this op with arguments A and then B, which differ only
    /// in number values, ends in the same state as applying it with B alone." Opt-in for absolute setters; a
    /// relative `x += amount` must keep the default (`false`).
    fn coalesces() -> bool
    where
        Self: Sized,
    {
        false
    }
}

/// Object-safe view of an [`Op`], used by the registry and executor to handle ops they don't know statically.
pub trait DynOp: Send + Sync {
    /// The op's static facts.
    fn meta(&self) -> OpMeta;
    /// See [`Op::plan`].
    ///
    /// # Errors
    /// As [`Op::plan`].
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError>;
    /// The arguments as JSON (for the op log and replays).
    ///
    /// # Errors
    /// Only if the op's own `Serialize` impl fails.
    fn args(&self) -> Result<Value, OpError>;
    /// See [`Op::work`].
    ///
    /// # Errors
    /// As [`Op::work`].
    fn work(&self, _job: &JobCtx<'_>) -> Result<Value, OpError> {
        Err(jobs::not_a_job(self.meta().name))
    }
    /// See [`Op::finish`].
    ///
    /// # Errors
    /// As [`Op::finish`].
    fn finish(&self, _ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        Ok(Plan::answer(result))
    }
    /// See [`Op::job_shape`].
    fn job_shape(&self) -> JobShape {
        JobShape::Plain
    }
}

impl<T: Op> DynOp for T {
    fn meta(&self) -> OpMeta {
        T::meta()
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Op::plan(self, ctx)
    }

    fn args(&self) -> Result<Value, OpError> {
        serde_json::to_value(self).map_err(|e| OpError::internal(format!("serialising {} failed: {e}", T::meta().name)))
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        Op::work(self, job)
    }

    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        Op::finish(self, ctx, result)
    }

    fn job_shape(&self) -> JobShape {
        T::job_shape()
    }
}

/// Registers an op type so every surface (MCP, CLI, scripts, palette, docs) picks it up. Put it next to the op.
///
/// ```ignore
/// omuzaki_ops_api::register_op!(SetFaceProfile);
/// ```
#[macro_export]
macro_rules! register_op {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::OpRegistration::of::<$ty>() }
        $crate::__record_registration!("op", $ty);
    };
}
