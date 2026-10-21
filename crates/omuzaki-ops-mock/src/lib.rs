//! A small but faithful in-memory [`Runner`] for tests: dry-run, grants, transactions, undo and redo, the op log and
//! events, over any document that implements [`DocWrite`]. It stands in for `omuzaki-ops` wherever a test needs a
//! runner without the executor (ADR 0054 §6): the MCP tests use it, and the CLI and UI tests can.
//!
//! The runner finds ops in the registry, so a test registers the ops it needs with `register_op!` in its own binary.
//! It builds every contract value through the constructors of `omuzaki-ops-api`, so a field or variant added there
//! does not edit this crate, and it makes every authorization, plan and history check through
//! [`omuzaki_ops_api::checks`], the executor's own copy, so the two cannot disagree on them.
//!
//! This is a test oracle, not an executor: no sessions, approvals, fences or jobs, and no effects outside the document
//! (a step holding one is refused).

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocWrite, Patch, Value};
use omuzaki_ops_api::{
    Actor, Event, Grants, OpCtx, OpError, OpInfo, OpKind, OpMeta, OpRecord, Outcome, Request, Runner, Scope, TxId,
    Undo, checks, find, registrations,
};

/// One transaction, open or finished and ready to undo or redo.
struct Step {
    tx: TxId,
    label: String,
    patch: Patch,
    /// Undo is per actor: only the owner can undo or redo this step.
    owner: Actor,
    /// The scopes undo and redo need ([`checks::step_scope`]).
    scopes: BTreeSet<Scope>,
    /// The first `Undo::Explicit` op that recorded an effect outside the document: this oracle runs no effects, so
    /// undo and redo refuse the step. An Explicit op without an effect changed nothing outside, and is undone by its
    /// patch, as the executor does.
    explicit: Option<&'static str>,
}

impl Step {
    fn new(tx: TxId, label: &str, owner: Actor) -> Self {
        Self { tx, label: label.to_owned(), patch: Patch::new(), owner, scopes: BTreeSet::new(), explicit: None }
    }

    /// Adds one applied op to the step; `effect` says whether it recorded an effect outside the document.
    fn absorb(&mut self, meta: OpMeta, patch: &Patch, effect: bool) {
        self.scopes.extend(checks::step_scope(meta.scope, meta.undo, patch));
        if meta.undo == Undo::Explicit && effect && self.explicit.is_none() {
            self.explicit = Some(meta.name);
        }
        self.patch.extend(patch.clone());
    }
}

struct Inner<D> {
    doc: D,
    clock: FakeClock,
    log: Vec<OpRecord>,
    open: HashMap<TxId, Step>,
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
    fn take_open(&mut self, tx: TxId, actor: &Actor) -> Result<Step, OpError> {
        self.check_open(tx, actor)?;
        self.open.remove(&tx).ok_or_else(|| OpError::internal("checked open a moment ago"))
    }

    /// Borrows an open transaction, enforcing that only its owner may touch it.
    fn open_of(&self, tx: TxId, actor: &Actor) -> Result<&Step, OpError> {
        self.check_open(tx, actor)?;
        self.open.get(&tx).ok_or_else(|| OpError::internal("checked open a moment ago"))
    }

    /// Adds an applied op to `tx` if it is open, or makes it a one-step transaction of its own.
    fn attach(&mut self, tx: Option<TxId>, actor: &Actor, meta: OpMeta, patch: &Patch, effect: bool) -> TxId {
        if let Some((tx, open)) = tx.and_then(|tx| self.open.get_mut(&tx).map(|open| (tx, open))) {
            open.absorb(meta, patch, effect);
            return tx;
        }
        let tx = TxId(self.next_tx);
        self.next_tx += 1;
        let mut step = Step::new(tx, meta.summary, actor.clone());
        step.absorb(meta, patch, effect);
        self.commit_step(step);
        tx
    }

    /// Records a finished step, if it belongs in history, and then clears its owner's redo history.
    fn commit_step(&mut self, step: Step) {
        if checks::in_history(&step.patch, step.explicit.is_some()) {
            self.redo.retain(|s| s.owner != step.owner);
            self.undo.push(step);
        }
    }

    fn emit(&mut self, event: &Event) {
        self.subscribers.retain(|s| s.send(event.clone()).is_ok());
    }

    /// Moves `actor`'s newest step from the undo stack to the redo stack (`forward` false) or back, as the executor
    /// does: a step the grants can't cover stays where it was; a step with an explicit op, or one that no longer
    /// applies, is dropped and the document left untouched.
    fn travel(&mut self, actor: &Actor, grants: &Grants, forward: bool) -> Result<Option<String>, OpError> {
        let verb = if forward { "redo" } else { "undo" };
        let from = if forward { &mut self.redo } else { &mut self.undo };
        let Some(at) = from.iter().rposition(|s| &s.owner == actor) else { return Ok(None) };
        checks::require_step_scopes(from[at].scopes.iter().copied(), grants, actor, verb, &from[at].label)?;
        let step = from.remove(at);
        if let Some(op) = step.explicit {
            return Err(checks::explicit_refused(verb, &step.label, op));
        }
        let patch = if forward { step.patch.clone() } else { step.patch.inverse() };
        self.doc
            .apply(&patch)
            .map_err(|e| OpError::conflict(e.to_string()).hint("later changes touch the same values"))?;
        let (version, label) = (self.doc.version(), step.label.clone());
        let (tx, owner) = (step.tx, step.owner.clone());
        if forward {
            self.emit(&Event::redone(tx, label.clone(), actor.clone(), owner, version));
            self.undo.push(step);
        } else {
            self.emit(&Event::undone(tx, label.clone(), actor.clone(), owner, version));
            self.redo.push(step);
        }
        Ok(Some(label))
    }
}

impl<D: DocWrite + Send> Runner for MockRunner<D> {
    fn ops(&self) -> Vec<OpInfo> {
        let mut ops: Vec<OpInfo> = registrations().map(checks::op_info).collect();
        ops.sort_by_key(|o| o.name);
        ops
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        // The executor's order of checks (`checks` module docs). Nothing runs without `read`, not even a dry-run.
        checks::require_run(&req.grants, &req.actor, Scope::Read, &req.op)?;
        let reg = find(&req.op).ok_or_else(|| OpError::invalid("op", format!("unknown op {}", req.op)))?;
        let op = (reg.decode)(req.args.clone())?;
        let meta = op.meta();
        // A job is not part of a transaction. (The mock runs a job's plan as the whole job, with no worker thread.)
        if meta.kind == OpKind::Job && req.tx.is_some() {
            return Err(OpError::invalid("tx", "a job cannot join a transaction")
                .hint("run the job without `tx`: its finish is one undo step of its own"));
        }
        let mut inner = self.inner();
        if let Some(tx) = req.tx {
            inner.check_open(tx, &req.actor)?;
        }
        let plan = checks::plan(&*op, &req, OpCtx::new(&inner.doc, &req.actor, &inner.clock))?;
        if checks::answers_only(meta, req.mode) {
            return Ok(Outcome::new(plan));
        }
        let args = checks::record_args(&*op)?;
        let version = checks::apply_patch(&mut inner.doc, &plan.patch)
            .map_err(|e| OpError::conflict(e.to_string()).hint("re-plan and retry"))?;
        let tx = inner.attach(req.tx, &req.actor, meta, &plan.patch, plan.effect.is_some());
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
        inner.open.insert(tx, Step::new(tx, label, actor.clone()));
        tx
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let mut inner = self.inner();
        let open = inner.take_open(tx, actor)?;
        let label = open.label.clone();
        inner.commit_step(open);
        inner.emit(&Event::tx_committed(tx, label, actor.clone()));
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

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner().travel(actor, grants, false)
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner().travel(actor, grants, true)
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        checks::require_read(grants, "subscribe to events")?;
        let (tx, rx) = channel();
        self.inner().subscribers.push(tx);
        Ok(rx)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        checks::require_read(grants, "read the document")?;
        Ok(self.inner().doc.get(path).cloned())
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        checks::require_read(grants, "read the op log")?;
        let skip = usize::try_from(after_seq).unwrap_or(usize::MAX);
        Ok(self.inner().log.iter().skip(skip).take(limit).cloned().collect())
    }
}
