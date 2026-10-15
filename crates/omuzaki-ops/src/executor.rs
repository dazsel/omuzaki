//! The executor: [`Runner`] over a document, the op registry, a clock and the history in [`State`].

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use omuzaki_core::time::Clock;
use omuzaki_doc_api::{DocPath, DocWrite, Value};
use omuzaki_ops_api::{
    Actor, DynOp, Event, Grants, Mode, OpCtx, OpError, OpInfo, OpKind, OpMeta, OpRecord, OpRegistration, Outcome, Plan,
    ProjectDir, Request, Runner, Scope, Services, TxId, registrations,
};

use crate::bus::Bus;
use crate::index::OpIndex;
use crate::state::{Edit, State};
use crate::sync::lock;
use crate::{ExecutorError, error};

/// Runs ops against one document. See the [crate docs](crate) for the behaviour.
pub struct Executor {
    ops: OpIndex,
    clock: Arc<dyn Clock>,
    /// What ops may reach beyond the document. Fixed before the executor starts: the field has no setter.
    services: Services,
    /// The open project's folder, for ops that touch files (`OpCtx::fs_access`). Fixed like `services`.
    project_dir: Option<ProjectDir>,
    /// The document and everything that must change with it. One lock: this is the authoring plane, where
    /// correctness beats concurrency, and a single lock makes "plan, apply, log" atomic by construction.
    state: Mutex<State>,
    bus: Bus,
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
        Ok(Self {
            ops: OpIndex::build(registrations)?,
            clock,
            services: Services::none(),
            project_dir: None,
            state: Mutex::new(State::new(doc)),
            bus: Bus::default(),
        })
    }

    /// The same executor, with `services` available to every op through
    /// [`OpCtx::service`](omuzaki_ops_api::OpCtx::service). It takes `self` by value, so the set is fixed before the
    /// first op runs and immutable from then on. Without it an executor has no services, and an op that asks for one
    /// fails with [`ErrorCode::Unavailable`](omuzaki_ops_api::ErrorCode::Unavailable).
    #[must_use]
    pub fn with_services(mut self, services: Services) -> Self {
        self.services = services;
        self
    }

    /// The same executor, told which folder holds the project, so that ops can check file paths against it with
    /// [`OpCtx::fs_access`](omuzaki_ops_api::OpCtx::fs_access): reading inside it needs only `read`, and relative paths
    /// are relative to it. Get the folder from [`ProjectDir::open`], which refuses a folder that would make `read`
    /// mean "everything" (a filesystem root) or that does not exist. It takes `self` by value like
    /// [`with_services`](Self::with_services). Without it, ops see no project folder: relative paths are refused and
    /// every file needs an `fs` root, so forgetting to wire it refuses access rather than widening it.
    #[must_use]
    pub fn with_project_dir(mut self, dir: ProjectDir) -> Self {
        self.project_dir = Some(dir);
        self
    }

    /// Plans `op` against the current document. Queries must not plan a patch: a query that did would otherwise
    /// change the document through an interface everyone assumes is read-only.
    fn plan(&self, state: &State, op: &dyn DynOp, meta: OpMeta, req: &Request) -> Result<Plan, OpError> {
        // The grants are the request's own, not the executor's: one executor serves a person at the UI and an agent
        // over MCP at once, and each op sees only what its caller may do.
        let mut ctx = OpCtx::new(state.doc(), &req.actor, &*self.clock)
            .with_services(&self.services)
            .with_grants(&req.grants)
            .with_dry_run(req.mode == Mode::DryRun);
        if let Some(dir) = &self.project_dir {
            ctx = ctx.with_project_dir(dir);
        }
        let plan = op.plan(&ctx)?;
        if meta.kind == OpKind::Query && !plan.patch.changes.is_empty() {
            return Err(OpError::internal(format!("{} is a query but planned a change to the document", meta.name))
                .hint("this is a bug in the op: queries return an answer and no patch"));
        }
        Ok(plan)
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
            return Ok(Outcome::new(plan));
        }
        let record = state.apply_edit(Edit {
            op: meta.name,
            scope: meta.scope,
            undo: meta.undo,
            args: op.args()?,
            patch: &plan.patch,
            actor: req.actor.clone(),
            cause: req.cause.clone(),
            tx: req.tx,
            at: self.clock.now(),
        })?;
        Ok(Outcome::new(plan).with_applied(record))
    }

    /// Sends the queued events. Called after the state lock is released, so no subscriber is ever called (or waited
    /// on) while it is held.
    fn publish(&self) {
        self.bus.deliver(|| lock(&self.state).take_outbox());
    }

    /// Runs `change` under the lock, then publishes whatever events it queued.
    fn with_state<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        let result = change(&mut lock(&self.state));
        self.publish();
        result
    }
}

/// The actor needs `scope` to run `op`.
fn require(grants: &Grants, actor: &Actor, scope: Scope, op: &str) -> Result<(), OpError> {
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
        Ok(self.bus.subscribe())
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        require_read(grants, "read the document")?;
        Ok(lock(&self.state).doc().get(path).cloned())
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        require_read(grants, "read the op log")?;
        let state = lock(&self.state);
        // Sequence numbers run 1, 2, 3, ... so the records after `after_seq` start at that index.
        let start = usize::try_from(after_seq).unwrap_or(usize::MAX);
        Ok(state.log().iter().skip(start).take(limit).cloned().collect())
    }
}
