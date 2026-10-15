//! The executor: [`Runner`] over a document, the op registry, a clock and the history in [`State`].

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_core::time::Clock;
use omuzaki_doc_api::{DocPath, DocWrite, Value};
use omuzaki_ops_api::{
    Actor, Cause, DynOp, Event, Grants, JobId, JobInfo, JobShape, Jobs, Mode, OpCtx, OpError, OpInfo, OpKind, OpMeta,
    OpRecord, OpRegistration, Outcome, Plan, ProjectDir, Request, Runner, Scope, Services, SessionId, TxId,
    registrations,
};

use crate::bus::Bus;
use crate::index::OpIndex;
use crate::jobs::Manager;
use crate::state::{Edit, State};
use crate::sync::lock;
use crate::{ExecutorError, error};

/// What a job thread needs from the executor, as shared handles: it outlives any borrow of the [`Executor`].
#[derive(Clone)]
pub(crate) struct Env {
    pub(crate) state: Arc<Mutex<State>>,
    pub(crate) bus: Arc<Bus>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) services: Arc<Services>,
    pub(crate) project_dir: Option<Arc<ProjectDir>>,
}

impl Env {
    /// The context an op plans (or finishes) in. The grants are the request's own, not the executor's: one executor
    /// serves a person at the UI and an agent over MCP at once, and each op sees only what its caller may do.
    pub(crate) fn op_ctx<'a>(
        &'a self,
        state: &'a State,
        actor: &'a Actor,
        grants: &'a Grants,
        dry_run: bool,
    ) -> OpCtx<'a> {
        let mut ctx = OpCtx::new(state.doc(), actor, &*self.clock)
            .with_services(&self.services)
            .with_grants(grants)
            .with_dry_run(dry_run);
        if let Some(dir) = &self.project_dir {
            ctx = ctx.with_project_dir(dir);
        }
        ctx
    }

    /// Applies `plan` as an edit by `actor`: the one path every op, and every job finish, lands through.
    pub(crate) fn apply(
        &self,
        state: &mut State,
        op: &dyn DynOp,
        by: (&Actor, &Option<Cause>, Option<TxId>),
        plan: &Plan,
    ) -> Result<OpRecord, OpError> {
        let meta = op.meta();
        let (actor, cause, tx) = by;
        state.apply_edit(Edit {
            op: meta.name,
            scope: meta.scope,
            undo: meta.undo,
            args: op.args()?,
            patch: &plan.patch,
            actor: actor.clone(),
            cause: cause.clone(),
            tx,
            at: self.clock.now(),
        })
    }

    /// Sends the queued events. Called after the state lock is released, so no subscriber is ever called (or waited
    /// on) while it is held.
    pub(crate) fn publish(&self) {
        self.bus.deliver(|| lock(&self.state).take_outbox());
    }
}

/// Runs ops against one document. See the [crate docs](crate) for the behaviour.
pub struct Executor {
    ops: OpIndex,
    /// The document and everything that must change with it (one lock: this is the authoring plane, where
    /// correctness beats concurrency, and a single lock makes "plan, apply, log" atomic by construction), the clock,
    /// and what ops may reach beyond the document. Fixed before the executor starts: the fields have no setters.
    env: Env,
    /// The job manager, built over `env` and `job_budget` when the executor is configured.
    jobs: Arc<Manager>,
    job_budget: u64,
}

impl Executor {
    /// An executor over `doc` that runs every op registered with
    /// [`register_op!`](omuzaki_ops_api::register_op), stamping the log from `clock`.
    ///
    /// # Errors
    /// [`ExecutorError::DuplicateOp`] if two registered ops share a name.
    pub fn new(doc: Box<dyn DocWrite>, clock: Arc<dyn Clock>) -> Result<Self, ExecutorError> {
        Self::with_registrations(doc, clock, registrations())
    }

    /// Like [`Executor::new`], but over an explicit set of registrations instead of everything linked into the
    /// binary: for tests and for hosts that expose a subset of ops.
    ///
    /// # Errors
    /// [`ExecutorError::DuplicateOp`] if two registrations share a name.
    pub fn with_registrations(
        doc: Box<dyn DocWrite>,
        clock: Arc<dyn Clock>,
        registrations: impl IntoIterator<Item = &'static OpRegistration>,
    ) -> Result<Self, ExecutorError> {
        let env = Env {
            state: Arc::new(Mutex::new(State::new(doc))),
            bus: Arc::new(Bus::default()),
            clock,
            services: Arc::new(Services::none()),
            project_dir: None,
        };
        let jobs = Manager::new(env.clone(), u64::MAX);
        Ok(Self { ops: OpIndex::build(registrations)?, env, jobs, job_budget: u64::MAX })
    }

    /// Rebuilds the job manager over the changed environment. Builders take `self` by value before any job starts,
    /// so there is nothing running to carry over.
    fn rewired(mut self, change: impl FnOnce(&mut Env)) -> Self {
        change(&mut self.env);
        self.jobs = Manager::new(self.env.clone(), self.job_budget);
        self
    }

    /// The same executor, with `services` available to every op through
    /// [`OpCtx::service`](omuzaki_ops_api::OpCtx::service). It takes `self` by value, so the set is fixed before the
    /// first op runs and immutable from then on. Without it an executor has no services, and an op that asks for one
    /// fails with [`ErrorCode::Unavailable`](omuzaki_ops_api::ErrorCode::Unavailable).
    #[must_use]
    pub fn with_services(self, services: Services) -> Self {
        self.rewired(|env| env.services = Arc::new(services))
    }

    /// The same executor, told which folder holds the project, so that ops can check file paths against it with
    /// [`OpCtx::fs_access`](omuzaki_ops_api::OpCtx::fs_access): reading inside it needs only `read`, and relative paths
    /// are relative to it. Get the folder from [`ProjectDir::open`], which refuses a folder that would make `read`
    /// mean "everything" (a filesystem root) or that does not exist. It takes `self` by value like
    /// [`with_services`](Self::with_services). Without it, ops see no project folder: relative paths are refused and
    /// every file needs an `fs` root, so forgetting to wire it refuses access rather than widening it.
    #[must_use]
    pub fn with_project_dir(self, dir: ProjectDir) -> Self {
        self.rewired(|env| env.project_dir = Some(Arc::new(dir)))
    }

    /// The same executor, with `usd_micros` (millionths of a dollar) as the most that jobs may reserve and spend in
    /// all (ADR 0036 decision 3). A job whose reservation would not fit is refused with `Limited`. Without it the
    /// budget is unlimited: the session rules, which own the real budget, come with the sessions task.
    #[must_use]
    pub fn with_job_budget(mut self, usd_micros: u64) -> Self {
        self.job_budget = usd_micros;
        self.jobs = Manager::new(self.env.clone(), usd_micros);
        self
    }

    /// Enforces the jobs' time caps and cancel deadlines against the executor's clock. A supervisor thread calls it
    /// about every 50 ms of wall time, so a host with the system clock never calls it; a host or test with a
    /// [`FakeClock`](omuzaki_core::time::FakeClock) calls it after moving the clock.
    pub fn poll_jobs(&self) {
        self.jobs.poll();
    }

    /// Cancels every job (`Shutdown`), refuses new ones and returns the money charged to jobs so far, in
    /// micro-dollars, counting every reservation still held in full: a thread that may still spend is charged for the
    /// most it could. It does not wait for the threads. Dropping the executor does the same.
    pub fn shutdown_jobs(&self) -> u64 {
        self.jobs.shutdown()
    }

    /// Blocks until at most `at_most` job threads are still running after their job ended (cancelled past its
    /// deadline and detached), so shutdown code and tests can wait for one to return without polling.
    pub fn wait_detached(&self, at_most: usize) {
        self.jobs.wait_detached(at_most);
    }

    /// Plans `op` against the current document. Queries must not plan a patch: a query that did would otherwise
    /// change the document through an interface everyone assumes is read-only.
    fn plan(&self, state: &State, op: &dyn DynOp, meta: OpMeta, req: &Request) -> Result<Plan, OpError> {
        let ctx = self.env.op_ctx(state, &req.actor, &req.grants, req.mode == Mode::DryRun);
        let plan = op.plan(&ctx)?;
        if meta.kind == OpKind::Query && !plan.patch.changes.is_empty() {
            return Err(OpError::internal(format!("{} is a query but planned a change to the document", meta.name))
                .hint("this is a bug in the op: queries return an answer and no patch"));
        }
        // A job changes the document once, at finish: a plan that already carries a patch would apply twice or not
        // at all, so it is refused here, in the dry-run too.
        if meta.kind == OpKind::Job && !plan.patch.changes.is_empty() {
            return Err(OpError::internal(format!("{} is a job but planned a change to the document", meta.name))
                .hint("this is a bug in the op: a job's plan returns no patch; its `finish` returns the change"));
        }
        Ok(plan)
    }

    /// The refusals that depend only on the request and the op: a job is not part of a transaction, and a driver job
    /// is not available yet.
    fn check_job_request(op: &dyn DynOp, req: &Request) -> Result<(), OpError> {
        if req.tx.is_some() {
            return Err(OpError::invalid("tx", "a job cannot join a transaction")
                .hint("run the job without `tx`: its finish is one undo step of its own"));
        }
        if op.job_shape() == JobShape::Driver {
            return Err(OpError::unsupported("driver jobs are not available yet"));
        }
        Ok(())
    }

    /// Plans a job under the lock and starts it on the manager.
    fn start_job(&self, op: Arc<dyn DynOp>, req: Request) -> Result<JobInfo, OpError> {
        let meta = op.meta();
        let plan = {
            let state = lock(&self.env.state);
            require(&req.grants, &req.actor, meta.scope, meta.name)?;
            self.plan(&state, &*op, meta, &req)?
        };
        self.jobs.start(op, req, plan)
    }

    /// The part of [`Runner::run`] that holds the lock.
    fn run_locked(&self, state: &mut State, op: &dyn DynOp, req: &Request) -> Result<Outcome, OpError> {
        let meta = op.meta();
        if let Some(tx) = req.tx {
            state.check_tx(tx, &req.actor)?;
        }
        // `read` was checked before the lookup. Applying also needs the op's own scope. A dry-run of an edit needs
        // only `read` (it is how an actor shows what it would ask permission for), but a query's plan *is* its answer,
        // so a dry-run of one is gated like the real thing.
        if req.mode == Mode::Apply || meta.kind == OpKind::Query {
            require(&req.grants, &req.actor, meta.scope, meta.name)?;
        }
        let plan = self.plan(state, op, meta, req)?;
        if req.mode == Mode::DryRun || meta.kind == OpKind::Query {
            return Ok(Outcome { plan, applied: None });
        }
        let record = self.env.apply(state, op, (&req.actor, &req.cause, req.tx), &plan)?;
        Ok(Outcome { plan, applied: Some(record) })
    }

    /// Runs `change` under the lock, then publishes whatever events it queued.
    fn with_state<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        let result = change(&mut lock(&self.env.state));
        self.env.publish();
        result
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.jobs.shutdown();
    }
}

/// The actor needs `scope` to run `op`.
pub(crate) fn require(grants: &Grants, actor: &Actor, scope: Scope, op: &str) -> Result<(), OpError> {
    if grants.allows(scope) { Ok(()) } else { Err(error::denied(actor, &format!("run {op}"), scope)) }
}

/// The reader needs `read` to `doing`. The grants alone say who is asking: reads carry no actor.
fn require_read(grants: &Grants, doing: &str) -> Result<(), OpError> {
    if grants.allows(Scope::Read) {
        Ok(())
    } else {
        Err(OpError::denied(format!("the caller may not {doing}: it needs the {} scope", Scope::Read))
            .hint(format!("grant the {} scope for this session, then retry", Scope::Read)))
    }
}

impl Runner for Executor {
    fn ops(&self) -> Vec<OpInfo> {
        self.ops.infos()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        // Plans and conflict messages carry document values, so nothing runs without `read`, not even a dry-run.
        require(&req.grants, &req.actor, Scope::Read, &req.op)?;
        let registration = self.ops.find(&req.op)?;
        let op = (registration.decode)(req.args.clone())?;
        if op.meta().kind == OpKind::Job {
            Self::check_job_request(&*op, &req)?;
            // A dry-run is the plan alone (below). An apply starts the job and waits, holding no lock: every other
            // actor keeps editing while it runs, and the finish lands through the same apply path as any edit.
            if req.mode == Mode::Apply {
                let info = self.start_job(Arc::from(op), req)?;
                return self.jobs.outcome(info.id);
            }
        }
        self.with_state(|state| self.run_locked(state, &*op, &req))
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.with_state(|state| state.begin(label, actor))
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.with_state(|state| state.commit(tx, actor))
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.with_state(|state| state.rollback(tx, actor))
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.with_state(|state| state.undo(actor, grants))
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.with_state(|state| state.redo(actor, grants))
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        require_read(grants, "subscribe to events")?;
        Ok(self.env.bus.subscribe())
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        require_read(grants, "read the document")?;
        Ok(lock(&self.env.state).doc().get(path).cloned())
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        require_read(grants, "read the op log")?;
        let state = lock(&self.env.state);
        // Sequence numbers run 1, 2, 3, ... so the records after `after_seq` start at that index.
        let start = usize::try_from(after_seq).unwrap_or(usize::MAX);
        Ok(state.log().iter().skip(start).take(limit).cloned().collect())
    }
}

impl Jobs for Executor {
    fn start(&self, req: Request) -> Result<JobInfo, OpError> {
        if req.mode != Mode::Apply {
            return Err(OpError::invalid("mode", "a job starts only in apply mode").hint("a dry-run is the plan alone"));
        }
        require(&req.grants, &req.actor, Scope::Read, &req.op)?;
        let registration = self.ops.find(&req.op)?;
        let op = (registration.decode)(req.args.clone())?;
        if op.meta().kind != OpKind::Job {
            return Err(OpError::invalid("op", format!("{} is not a job", req.op)).hint("run it with `Runner::run`"));
        }
        Self::check_job_request(&*op, &req)?;
        self.start_job(Arc::from(op), req)
    }

    fn job(&self, id: JobId, grants: &Grants) -> Result<Option<JobInfo>, OpError> {
        require_read(grants, "read a job")?;
        Ok(self.jobs.job(id))
    }

    fn jobs(&self, grants: &Grants) -> Result<Vec<JobInfo>, OpError> {
        require_read(grants, "list jobs")?;
        Ok(self.jobs.list())
    }

    fn cancel(&self, id: JobId, by: &Actor) -> Result<JobInfo, OpError> {
        self.jobs.cancel(id, by)
    }

    fn cancel_session(&self, session: SessionId, _by: &Actor) -> Result<Vec<JobId>, OpError> {
        // Sessions are the session task's: until they exist, no session id is known.
        Err(OpError::not_found(format!("session {} is not known", session.0)))
    }

    fn wait(&self, id: JobId, timeout: Option<Duration>) -> Result<JobInfo, OpError> {
        self.jobs.wait(id, timeout)
    }
}
