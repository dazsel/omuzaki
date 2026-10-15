//! A small but faithful in-memory [`Runner`] for tests: dry-run, grants, transactions, undo and redo, the op log and
//! events, over any document that implements [`DocWrite`]. It stands in for `omuzaki-ops` wherever a test needs a
//! runner without the executor (ADR 0054 §6): the MCP tests use it, and the CLI and UI tests can.
//!
//! The runner finds ops in the registry, so a test registers the ops it needs with `register_op!` in its own binary.
//! It builds every contract value through the constructors of `omuzaki-ops-api`, so a field or variant added there
//! does not edit this crate.
//!
//! This is a test oracle, not an executor: no sessions, approvals, fences or jobs.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocWrite, Patch, Value};
use omuzaki_ops_api::{
    Actor, Event, Grants, Mode, OpCtx, OpError, OpInfo, OpKind, OpRecord, Outcome, Request, Runner, Scope, TxId, find,
    registrations,
};

/// One finished transaction, ready to undo or redo.
struct Step {
    tx: TxId,
    label: String,
    patch: Patch,
    /// Undo is per actor: only the owner can undo or redo this step.
    owner: Actor,
}

struct OpenTx {
    label: String,
    owner: Actor,
    patch: Patch,
}

struct Inner<D> {
    doc: D,
    clock: FakeClock,
    log: Vec<OpRecord>,
    open: HashMap<TxId, OpenTx>,
    next_tx: u64,
    undo: Vec<Step>,
    redo: Vec<Step>,
    subscribers: Vec<Sender<Event>>,
}

/// An in-memory [`Runner`] over the document `D`. Time stands still at zero on a [`FakeClock`], so records are
/// reproducible.
pub struct MockRunner<D>(Mutex<Inner<D>>);

impl<D: DocWrite + Send> MockRunner<D> {
    /// A runner over `doc`.
    pub fn new(doc: D) -> Self {
        Self(Mutex::new(Inner {
            doc,
            clock: FakeClock::new(Seconds(0.0)),
            log: Vec::new(),
            open: HashMap::new(),
            next_tx: 1,
            undo: Vec::new(),
            redo: Vec::new(),
            subscribers: Vec::new(),
        }))
    }

    fn inner(&self) -> MutexGuard<'_, Inner<D>> {
        // An op that panics while planning poisons the lock; the document itself is untouched, so carry on.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The op log so far.
    pub fn records(&self) -> Vec<OpRecord> {
        self.inner().log.clone()
    }

    /// True while a transaction is open.
    pub fn has_open_tx(&self) -> bool {
        !self.inner().open.is_empty()
    }
}

impl<D: DocWrite> Inner<D> {
    /// Checks that `tx` is open and belongs to `actor`.
    fn check_open(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        match self.open.get(&tx) {
            None => Err(OpError::not_found(format!("transaction {} isn't open", tx.0))),
            Some(open) if &open.owner != actor => {
                Err(OpError::denied(format!("transaction {} belongs to another actor", tx.0)))
            }
            Some(_) => Ok(()),
        }
    }

    /// Removes an open transaction, enforcing that only its owner may finish it.
    fn take_open(&mut self, tx: TxId, actor: &Actor) -> Result<OpenTx, OpError> {
        self.check_open(tx, actor)?;
        self.open.remove(&tx).ok_or_else(|| OpError::internal("checked open a moment ago"))
    }

    /// Borrows an open transaction, enforcing that only its owner may touch it.
    fn open_of(&self, tx: TxId, actor: &Actor) -> Result<&OpenTx, OpError> {
        self.check_open(tx, actor)?;
        self.open.get(&tx).ok_or_else(|| OpError::internal("checked open a moment ago"))
    }

    /// Adds an applied patch to `tx` if it is open, or makes it a one-step transaction of its own.
    fn attach(&mut self, tx: Option<TxId>, actor: &Actor, label: &str, patch: &Patch) -> TxId {
        if let Some((tx, open)) = tx.and_then(|tx| self.open.get_mut(&tx).map(|open| (tx, open))) {
            open.patch.extend(patch.clone());
            return tx;
        }
        let tx = TxId(self.next_tx);
        self.next_tx += 1;
        self.commit_step(Step { tx, label: label.to_owned(), patch: patch.clone(), owner: actor.clone() });
        tx
    }

    /// Records a finished step and clears its owner's redo history.
    fn commit_step(&mut self, step: Step) {
        self.redo.retain(|s| s.owner != step.owner);
        self.undo.push(step);
    }

    fn emit(&mut self, event: &Event) {
        self.subscribers.retain(|s| s.send(event.clone()).is_ok());
    }

    fn apply_undo_step(&mut self, step: &Step, forward: bool) -> Result<(), OpError> {
        let patch = if forward { step.patch.clone() } else { step.patch.inverse() };
        self.doc
            .apply(&patch)
            .map(drop)
            .map_err(|e| OpError::conflict(e.to_string()).hint("later changes touch the same values"))
    }
}

/// The refusal for a request without `read`, worded as the executor words it.
fn require_read_for(grants: &Grants, op: &str) -> Result<(), OpError> {
    if grants.allows(Scope::Read) {
        Ok(())
    } else {
        Err(OpError::denied(format!("{op} needs the `read` scope"))
            .hint("ask the user to grant the `read` scope for this session"))
    }
}

fn require_read(grants: &Grants) -> Result<(), OpError> {
    if grants.allows(Scope::Read) { Ok(()) } else { Err(OpError::denied("needs the `read` scope")) }
}

impl<D: DocWrite + Send> Runner for MockRunner<D> {
    fn ops(&self) -> Vec<OpInfo> {
        let mut ops: Vec<OpInfo> =
            registrations().map(|r| OpInfo::new((r.meta)(), (r.schema)()).with_coalesces((r.coalesces)())).collect();
        ops.sort_by_key(|o| o.name);
        ops
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        // The executor's order of checks. Nothing runs without `read`, not even a dry-run.
        require_read_for(&req.grants, &req.op)?;
        let reg = find(&req.op).ok_or_else(|| OpError::invalid("op", format!("unknown op {}", req.op)))?;
        let op = (reg.decode)(req.args.clone())?;
        let meta = op.meta();
        // The log keeps the decoded op's canonical arguments (defaults filled, aliases resolved), as the executor does.
        let args = op.args()?;
        // A job is not part of a transaction. (The mock runs a job's plan as the whole job, with no worker thread.)
        if meta.kind == OpKind::Job && req.tx.is_some() {
            return Err(OpError::invalid("tx", "a job cannot join a transaction")
                .hint("run the job without `tx`: its finish is one undo step of its own"));
        }
        let mut inner = self.inner();
        if let Some(tx) = req.tx {
            inner.check_open(tx, &req.actor)?;
        }
        // Applying needs the op's own scope, checked before the op is asked for a plan. A dry-run of an edit needs only
        // `read`, but a query's plan *is* its answer, so a dry-run of one is gated like the real thing.
        if (req.mode == Mode::Apply || meta.kind == OpKind::Query) && !req.grants.allows(meta.scope) {
            return Err(OpError::denied(format!("{} needs the `{}` scope", meta.name, meta.scope))
                .hint(format!("ask the user to grant the `{}` scope for this session", meta.scope)));
        }
        // As the executor builds it: the request's grants (a composite op's `plan_step` checks them) and its mode.
        let ctx = OpCtx::new(&inner.doc, &req.actor, &inner.clock)
            .with_grants(&req.grants)
            .with_dry_run(req.mode == Mode::DryRun);
        let plan = op.plan(&ctx)?;
        if meta.kind == OpKind::Query && !plan.patch.changes.is_empty() {
            return Err(OpError::internal(format!("{} is a query but planned a change to the document", meta.name))
                .hint("this is a bug in the op: queries return an answer and no patch"));
        }
        if meta.kind == OpKind::Job && !plan.patch.changes.is_empty() {
            return Err(OpError::internal(format!("{} is a job but planned a change to the document", meta.name))
                .hint("this is a bug in the op: a job's plan returns no patch; its `finish` returns the change"));
        }
        if req.mode == Mode::DryRun || meta.kind == OpKind::Query {
            return Ok(Outcome::new(plan));
        }
        let version =
            inner.doc.apply(&plan.patch).map_err(|e| OpError::conflict(e.to_string()).hint("re-plan and retry"))?;
        let tx = inner.attach(req.tx, &req.actor, meta.summary, &plan.patch);
        let record = OpRecord::new(
            inner.log.len() as u64 + 1,
            req.op,
            args,
            req.actor,
            tx,
            Clock::now(&inner.clock),
            plan.patch.clone(),
            version,
        )
        .with_cause(req.cause);
        inner.log.push(record.clone());
        inner.emit(&Event::op_applied(record.clone()));
        Ok(Outcome::new(plan).with_applied(record))
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        let mut inner = self.inner();
        let tx = TxId(inner.next_tx);
        inner.next_tx += 1;
        inner.open.insert(tx, OpenTx { label: label.to_owned(), owner: actor.clone(), patch: Patch::new() });
        tx
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let mut inner = self.inner();
        let open = inner.take_open(tx, actor)?;
        inner.commit_step(Step { tx, label: open.label.clone(), patch: open.patch, owner: open.owner });
        inner.emit(&Event::tx_committed(tx, open.label, actor.clone()));
        Ok(())
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let mut inner = self.inner();
        // Like the executor, a rollback that conflicts leaves the transaction open for its owner to resolve or commit:
        // it is removed only once its inverse has applied.
        let inverse = inner.open_of(tx, actor)?.patch.inverse();
        let version = inner.doc.apply(&inverse).map_err(|e| OpError::conflict(e.to_string()))?;
        inner.open.remove(&tx);
        inner.emit(&Event::tx_rolled_back(tx, actor.clone(), version));
        Ok(())
    }

    fn undo(&self, actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        let mut inner = self.inner();
        let Some(at) = inner.undo.iter().rposition(|s| &s.owner == actor) else { return Ok(None) };
        let step = inner.undo.remove(at);
        inner.apply_undo_step(&step, false)?;
        let version = inner.doc.version();
        inner.emit(&Event::undone(step.tx, step.label.clone(), actor.clone(), step.owner.clone(), version));
        let label = step.label.clone();
        inner.redo.push(step);
        Ok(Some(label))
    }

    fn redo(&self, actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        let mut inner = self.inner();
        let Some(at) = inner.redo.iter().rposition(|s| &s.owner == actor) else { return Ok(None) };
        let step = inner.redo.remove(at);
        inner.apply_undo_step(&step, true)?;
        let version = inner.doc.version();
        inner.emit(&Event::redone(step.tx, step.label.clone(), actor.clone(), step.owner.clone(), version));
        let label = step.label.clone();
        inner.undo.push(step);
        Ok(Some(label))
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        require_read(grants)?;
        let (tx, rx) = channel();
        self.inner().subscribers.push(tx);
        Ok(rx)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        require_read(grants)?;
        Ok(self.inner().doc.get(path).cloned())
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        require_read(grants)?;
        let skip = usize::try_from(after_seq).unwrap_or(usize::MAX);
        Ok(self.inner().log.iter().skip(skip).take(limit).cloned().collect())
    }
}
