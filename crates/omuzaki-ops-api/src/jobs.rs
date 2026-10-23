//! Jobs: long-running ops that run off the document lock, with progress, cancellation and caps (ADR 0036, design
//! `ops-api.md` §8). This module is the contract only; the job manager in `omuzaki-ops` implements [`Jobs`].
//!
//! A job is three calls: [`Op::plan`](crate::Op::plan) validates and prices under the lock, [`Op::work`] runs on a job
//! worker without it (reading the document only through [`JobCtx::read`]), and [`Op::finish`] runs under the lock and
//! returns the [`Plan`](crate::Plan) the executor applies, so the document changes once, at finish.

use std::sync::Arc;
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocRead, Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::NO_GRANTS;
use crate::services::{NO_SERVICES, Service, Services};
use crate::{
    Actor, CanonicalPath, Cause, CostHint, FsMode, Grants, Mode, OpError, Outcome, ProjectDir, Request, SessionId,
    TxId, file_access,
};

/// Identifies one job. Project-wide and never reused: recovery continues the count.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct JobId(pub u64);

/// The worker pool a job runs in; it decides how many jobs run at once.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Processor work, the default.
    Cpu,
    /// Needs GPU memory (`CostHint.gpu_mib` set).
    Gpu,
    /// Network or paid work (scope `net`, or `CostHint.usd` set).
    Net,
}

/// Why a job was cancelled.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// Someone called [`Jobs::cancel`].
    Requested,
    /// The session was stopped.
    Stop,
    /// The session ended or was revoked.
    SessionEnded,
    /// The job passed its time cap.
    Timeout,
    /// The job passed its money cap.
    Budget,
    /// The project was closing.
    Shutdown,
}

/// Where a job is in its life. Every change is an [`Event::JobChanged`](crate::Event::JobChanged).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for a worker in its lane.
    Queued,
    /// `work` is running.
    Running,
    /// `work` is done, the session is paused and `finish` waits for it to resume.
    Held,
    /// `finish` ran and the job ended.
    #[non_exhaustive]
    Done {
        /// The transaction the finish went into.
        tx: TxId,
        /// The finish's op-log record; `None` if it changed nothing.
        seq: Option<u64>,
        /// What the op returned: the same [`Outcome`] a blocking [`Runner::run`](crate::Runner::run) of the op returns,
        /// so a client that polls renders the same result as one that waited. Held in memory only: it is not
        /// serialised (a journal entry or an event log would carry every job's patch), so it is `None` after a
        /// round trip and for a job recovered from a journal. Build one with [`JobState::done`] and
        /// [`JobState::with_outcome`].
        #[serde(skip)]
        #[schemars(skip)]
        outcome: Option<Arc<Outcome>>,
    },
    /// The job ended with an error.
    Failed {
        /// What went wrong.
        error: OpError,
    },
    /// The job was cancelled.
    Cancelled {
        /// Who cancelled it; `None` when the system did (a cap, shutdown).
        by: Option<Actor>,
        /// Why.
        reason: CancelReason,
    },
    /// The job was running at a crash. Set by recovery; jobs are never re-run.
    Interrupted,
}

impl JobState {
    /// A finished job whose finish went into `tx` and wrote log record `seq` (`None` if it changed nothing). It carries
    /// no outcome; add one with [`JobState::with_outcome`].
    #[must_use]
    pub fn done(tx: TxId, seq: Option<u64>) -> Self {
        Self::Done { tx, seq, outcome: None }
    }

    /// This state carrying `outcome`, when it is [`JobState::Done`]; any other state is returned unchanged.
    #[must_use]
    pub fn with_outcome(mut self, outcome: Arc<Outcome>) -> Self {
        if let Self::Done { outcome: slot, .. } = &mut self {
            *slot = Some(outcome);
        }
        self
    }

    /// What the op returned, for a finished job that kept it (see [`JobState::Done`]).
    #[must_use]
    pub fn outcome(&self) -> Option<&Outcome> {
        match self {
            Self::Done { outcome, .. } => outcome.as_deref(),
            _ => None,
        }
    }
}

/// A snapshot of one job, for lists, events and `wait`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobInfo {
    /// The job.
    pub id: JobId,
    /// The op it runs.
    pub op: String,
    /// A label for humans (the transaction label of a driver job).
    pub label: String,
    /// Who started it.
    pub actor: Actor,
    /// The person the acting session works for.
    pub on_behalf_of: Option<Actor>,
    /// The session it belongs to, for agents and plugins.
    pub session: Option<SessionId>,
    /// Why it was started.
    pub cause: Option<Cause>,
    /// Its lane.
    pub lane: Lane,
    /// Where it is now.
    pub state: JobState,
    /// What it is doing, as last reported by [`JobCtx::progress`].
    pub stage: Option<String>,
    /// How far along, 0 to 1.
    pub fraction: f32,
    /// The plan's cost estimate.
    pub estimate: Option<CostHint>,
    /// Money reported so far, in micro-dollars.
    pub spent_usd_micros: u64,
    /// When it was queued, on the runner's clock.
    pub queued_at: Seconds,
    /// When `work` began.
    pub started_at: Option<Seconds>,
    /// When it ended.
    pub ended_at: Option<Seconds>,
}

/// The job manager, implemented by the executor like [`Sessions`](crate::Sessions). [`Runner::run`](crate::Runner::run)
/// of a job op waits for it to end; this trait starts jobs without waiting.
pub trait Jobs: Send + Sync {
    /// Starts a job. `req.mode` must be [`Mode::Apply`]. Returns once `JobStarted` is durable, before `work` begins.
    ///
    /// # Errors
    /// As [`Runner::run`](crate::Runner::run), plus `Invalid` for an op that is not a job, and `Limited` over a queue
    /// limit.
    fn start(&self, req: Request) -> Result<JobInfo, OpError>;

    /// One job, live or recently ended. Needs `read`.
    ///
    /// # Errors
    /// `Denied` if `grants` lack [`Scope::Read`](crate::Scope::Read).
    fn job(&self, id: JobId, grants: &Grants) -> Result<Option<JobInfo>, OpError>;

    /// The live jobs, then the last 100 ended. Needs `read`.
    ///
    /// # Errors
    /// `Denied` if `grants` lack [`Scope::Read`](crate::Scope::Read).
    fn jobs(&self, grants: &Grants) -> Result<Vec<JobInfo>, OpError>;

    /// Asks a job to stop. Idempotent once it has ended.
    ///
    /// # Errors
    /// `NotFound` for an unknown job; `Denied` if `by` may not cancel it.
    fn cancel(&self, id: JobId, by: &Actor) -> Result<JobInfo, OpError>;

    /// Cancels every live job of `session`; returns their ids.
    ///
    /// # Errors
    /// `NotFound` for an unknown session; `Denied` if `by` may not.
    fn cancel_session(&self, session: SessionId, by: &Actor) -> Result<Vec<JobId>, OpError>;

    /// Blocks until the job ends or `timeout` passes, then returns its snapshot (still live on a timeout).
    ///
    /// # Errors
    /// `NotFound` for an unknown job.
    fn wait(&self, id: JobId, timeout: Option<Duration>) -> Result<JobInfo, OpError>;
}

/// How a job op uses [`JobCtx`].
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobShape {
    /// `work` computes a result; the document changes only through `finish`.
    Plain,
    /// `work` edits the document through [`JobCtx::call`]. The Runner contract for a driver job:
    /// - every inner request runs inside the one transaction the executor opens at start (or joins the caller's `tx`);
    /// - the whole job is one undo entry;
    /// - inner [`OpApplied`](crate::Event::OpApplied) events are emitted only when the transaction commits;
    /// - cancellation or an error rolls back everything the job did (a joined transaction: only the job's own edits);
    /// - each inner request is scope-checked against the job's grants at that call, so the job cannot reach past them.
    Driver,
}

/// The executor's handle for a driver job's inner requests, behind [`JobCtx::call`].
pub trait JobCalls: Send + Sync {
    /// Runs one inner request as the job's actor, with the job's grants, cause and transaction. It takes the document
    /// lock for this call only. It obeys the driver-job contract on [`JobShape::Driver`]: it runs inside the job's one
    /// transaction (one undo entry), its `OpApplied` event waits for the commit, a cancel or error rolls it back, and
    /// it is scope-checked against the job's grants at this call.
    ///
    /// # Errors
    /// As [`Runner::run`](crate::Runner::run), plus `Cancelled` once the job is cancelled.
    fn call(&self, op: &str, args: Value, mode: Mode) -> Result<Outcome, OpError>;
}

/// The document as a job's worker sees it: a short read under the lock.
pub trait JobDoc: Send + Sync {
    /// Calls `f` with the document, holding the lock only for the call.
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead));
}

/// The cooperative cancel handle, the never-cancelled handle and the settable flag a job's work polls, defined once in
/// [`omuzaki_core::cancel`]: a job's [`CancelFlag`] is the one an HTTP transport or a script run takes.
pub use omuzaki_core::cancel::{Cancel, CancelFlag, NeverCancel};

/// What the executor lets a running job report and ask.
pub trait JobSignal: Send + Sync {
    /// Reports the current `stage` and `fraction` (0 to 1). The executor coalesces events (at most 10 a second).
    fn progress(&self, stage: &str, fraction: f32);
    /// Whether the job was cancelled. Must agree with [`cancel_flag`](Self::cancel_flag).
    fn is_cancelled(&self) -> bool;
    /// An owned handle on the job's cancellation flag, for work that outlives the [`JobCtx`] borrow.
    fn cancel_flag(&self) -> CancelFlag;
    /// Reports money spent, in micro-dollars.
    ///
    /// # Errors
    /// `Limited` once the total would pass the job's money cap.
    fn spend(&self, usd_micros: u64) -> Result<(), OpError>;
}

/// What a job's `work` can see and do: the document (briefly), services, files, progress, cancellation and money.
///
/// `#[non_exhaustive]` so it grows without breaking callers: build one with [`JobCtx::new`] and the `with_*` methods.
#[non_exhaustive]
pub struct JobCtx<'a> {
    id: JobId,
    actor: &'a Actor,
    doc: &'a dyn JobDoc,
    signal: &'a dyn JobSignal,
    services: &'a Services,
    grants: &'a Grants,
    cause: Option<&'a Cause>,
    project_dir: Option<&'a ProjectDir>,
    calls: Option<&'a dyn JobCalls>,
    plan_output: Option<&'a Value>,
    reserved_usd_micros: u64,
}

impl<'a> JobCtx<'a> {
    /// A context for job `id` run by `actor`, with no services, no grants, no project folder and no inner calls.
    pub fn new(id: JobId, actor: &'a Actor, doc: &'a dyn JobDoc, signal: &'a dyn JobSignal) -> Self {
        Self {
            id,
            actor,
            doc,
            signal,
            services: &NO_SERVICES,
            grants: &NO_GRANTS,
            cause: None,
            project_dir: None,
            calls: None,
            plan_output: None,
            reserved_usd_micros: 0,
        }
    }

    /// The same context with the app's `services`.
    #[must_use]
    pub fn with_services(mut self, services: &'a Services) -> Self {
        self.services = services;
        self
    }

    /// The same context acting with `grants`, which [`fs_access`](Self::fs_access) checks paths against.
    #[must_use]
    pub fn with_grants(mut self, grants: &'a Grants) -> Self {
        self.grants = grants;
        self
    }

    /// The same context carrying the request's `cause`, so work that calls on someone else's behalf (a plugin command,
    /// ADR 0058) can label what it does with why it ran.
    #[must_use]
    pub fn with_cause(mut self, cause: &'a Cause) -> Self {
        self.cause = Some(cause);
        self
    }

    /// The same context with the open project's folder.
    #[must_use]
    pub fn with_project_dir(mut self, dir: &'a ProjectDir) -> Self {
        self.project_dir = Some(dir);
        self
    }

    /// The same context with the executor's handle for inner requests. Set for driver jobs only.
    #[must_use]
    pub fn with_calls(mut self, calls: &'a dyn JobCalls) -> Self {
        self.calls = Some(calls);
        self
    }

    /// The same context carrying what `plan` returned as its output, so `work` runs what the plan chose and priced.
    #[must_use]
    pub fn with_plan_output(mut self, output: &'a Value) -> Self {
        self.plan_output = Some(output);
        self
    }

    /// The same context carrying the money reserved at start, in micro-dollars.
    #[must_use]
    pub fn with_reserved_usd_micros(mut self, micros: u64) -> Self {
        self.reserved_usd_micros = micros;
        self
    }

    /// This job's id.
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Who started the job.
    pub fn actor(&self) -> &'a Actor {
        self.actor
    }

    /// The grants the context acts with: those given to [`with_grants`](Self::with_grants), or none.
    pub fn grants(&self) -> &'a Grants {
        self.grants
    }

    /// The request's cause, when [`with_cause`](Self::with_cause) set one. An `Option` because a request's cause is
    /// optional too ([`Request`](crate::Request)'s `cause`).
    pub fn cause(&self) -> Option<&'a Cause> {
        self.cause
    }

    /// Reads the document. The lock is held only while `f` runs, so keep it short and copy out what `work` needs.
    pub fn read<R>(&self, f: impl FnOnce(&dyn DocRead) -> R) -> R {
        let mut f = Some(f);
        let mut out = None;
        self.doc.read(&mut |doc| {
            if let Some(f) = f.take() {
                out = Some(f(doc));
            }
        });
        // `JobDoc::read` is documented to call `f` exactly once, so `out` is set. The fallback keeps the library
        // free of `unwrap`: it is unreachable for a conforming implementation.
        match out {
            Some(value) => value,
            None => unreachable_doc_read(),
        }
    }

    /// The app's implementation of the API trait `S`, as [`OpCtx::service`](crate::OpCtx::service).
    ///
    /// # Errors
    /// `Unavailable`, naming the trait, when the app did not register `S`.
    pub fn service<S: ?Sized + Service>(&self) -> Result<&'a S, OpError> {
        self.services.get::<S>().ok_or_else(|| OpError::unavailable(std::any::type_name::<S>()))
    }

    /// Every service the job runs with, for an [`OpCtx`](crate::OpCtx) over [`read`](Self::read)'s document when the
    /// work reuses a check that an op's `plan` also runs (`io.export` runs `io.check_export`'s).
    pub fn services(&self) -> &'a Services {
        self.services
    }

    /// Asks whether the job may touch `path` and resolves it, exactly as [`OpCtx::fs_access`](crate::OpCtx::fs_access).
    ///
    /// # Errors
    /// As [`OpCtx::fs_access`](crate::OpCtx::fs_access).
    pub fn fs_access(&self, path: &str, mode: FsMode) -> Result<CanonicalPath, OpError> {
        file_access::check(self.grants, self.project_dir, path, mode)
    }

    /// The job's scratch folder, `.omuzaki/jobs/<id>/` in the project, opened in [`FsMode::Write`]. A derived path:
    /// the executor deletes it when the job ends and recovery deletes leftovers.
    ///
    /// # Errors
    /// As [`fs_access`](Self::fs_access).
    pub fn scratch(&self) -> Result<CanonicalPath, OpError> {
        self.fs_access(&format!(".omuzaki/jobs/{}", self.id.0), FsMode::Write)
    }

    /// The output `plan` produced; `None` if the executor did not set it. `work` runs what the plan chose and
    /// priced, and never chooses again.
    pub fn plan_output(&self) -> Option<&'a Value> {
        self.plan_output
    }

    /// The money reserved at start, in micro-dollars: the cap [`spend`](Self::spend) already enforces. 0 if not set.
    pub fn reserved_usd_micros(&self) -> u64 {
        self.reserved_usd_micros
    }

    /// An owned handle on the job's cancellation flag, still valid after the `JobCtx` borrow ends.
    pub fn cancel_flag(&self) -> CancelFlag {
        self.signal.cancel_flag()
    }

    /// Reports progress: what the job is doing and how far along (0 to 1).
    pub fn progress(&self, stage: &str, fraction: f32) {
        self.signal.progress(stage, fraction);
    }

    /// Whether the job was cancelled; long loops poll it.
    pub fn is_cancelled(&self) -> bool {
        self.signal.is_cancelled()
    }

    /// Reports progress, then fails if the job was cancelled. Call it between units of work.
    ///
    /// # Errors
    /// [`ErrorCode::Cancelled`](crate::ErrorCode::Cancelled). The executor adds the detail naming who and why.
    pub fn checkpoint(&self, stage: &str, fraction: f32) -> Result<(), OpError> {
        self.progress(stage, fraction);
        if self.is_cancelled() { Err(OpError::cancelled(self.id)) } else { Ok(()) }
    }

    /// Reports money spent, in micro-dollars, as it happens.
    ///
    /// # Errors
    /// `Limited` once the total would pass the job's money cap; the job should stop.
    pub fn spend(&self, usd_micros: u64) -> Result<(), OpError> {
        self.signal.spend(usd_micros)
    }

    /// Sends an inner request through the executor as the job's actor. Driver jobs only ([`JobShape::Driver`]).
    ///
    /// The driver-job contract: the request runs inside the job's one transaction (the whole job is one undo entry),
    /// its `OpApplied` event is emitted only when that transaction commits, a cancel or error rolls back everything
    /// the job did, and the request is scope-checked against the job's grants at this call.
    ///
    /// # Errors
    /// `Unsupported` in a plain job; otherwise as [`JobCalls::call`].
    pub fn call(&self, op: &str, args: Value, mode: Mode) -> Result<Outcome, OpError> {
        match self.calls {
            Some(calls) => calls.call(op, args, mode),
            None => Err(OpError::unsupported("only a driver job can call other ops")
                .hint("declare `job_shape() -> JobShape::Driver` on the op")),
        }
    }
}

#[cold]
fn unreachable_doc_read() -> ! {
    // A `JobDoc` that never calls its closure breaks the trait's contract; there is no value to return.
    #[allow(clippy::panic)]
    {
        panic!("JobDoc::read did not call its closure")
    }
}

/// The message of the default [`Op::work`]: the op is not a job.
pub(crate) fn not_a_job(op: &str) -> OpError {
    OpError::unsupported(format!("{op} is not a job"))
}
