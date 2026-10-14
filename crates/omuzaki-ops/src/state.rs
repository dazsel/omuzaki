//! The executor's mutable core: the document, the op log, open transactions and each actor's undo/redo stacks.
//!
//! Everything here runs under the executor's single lock and is synchronous. Events aren't sent from here: they're
//! queued in an outbox, which the executor drains after releasing the lock (see [`Bus::deliver`](crate::bus::Bus)).

use std::collections::{BTreeSet, HashMap};

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch, Value};
use omuzaki_ops_api::{Actor, Cause, Event, Grants, OpError, OpRecord, Scope, TxId};

use crate::error;

/// One undo step: everything a transaction changed, as one combined patch.
///
/// The inverse of a concatenation is the reversed concatenation of the inverses, so `patch.inverse()` undoes the
/// whole step exactly and atomically.
struct Step {
    tx: TxId,
    label: String,
    patch: Patch,
    /// The scopes of the ops that changed the document: undoing or redoing needs the same permissions.
    scopes: BTreeSet<Scope>,
}

impl Step {
    fn new(tx: TxId, label: &str) -> Self {
        Self { tx, label: label.to_owned(), patch: Patch::new(), scopes: BTreeSet::new() }
    }

    /// Adds one applied op. Ops that changed nothing (queries inside a transaction, no-op edits) add no scope.
    fn absorb(&mut self, scope: Scope, patch: &Patch) {
        if !patch.is_empty() {
            self.scopes.insert(scope);
        }
        self.patch.extend(patch.clone());
    }

    fn changes_document(&self) -> bool {
        !self.patch.is_empty()
    }

    fn require(&self, actor: &Actor, grants: &Grants, verb: &str) -> Result<(), OpError> {
        match self.scopes.iter().find(|scope| !grants.allows(**scope)) {
            Some(scope) => Err(error::denied(actor, &format!("{verb} {:?}", self.label), *scope)),
            None => Ok(()),
        }
    }
}

/// A transaction that has begun and not yet committed or rolled back.
struct OpenTx {
    owner: Actor,
    step: Step,
}

/// One actor's undo and redo stacks: undo is per actor, so an agent never undoes a person's edit by accident.
struct History {
    actor: Actor,
    undo: Vec<Step>,
    redo: Vec<Step>,
}

/// Which way [`State::travel`] moves along an actor's history.
#[derive(Copy, Clone)]
enum Direction {
    Undo,
    Redo,
}

impl Direction {
    fn verb(self) -> &'static str {
        match self {
            Direction::Undo => "undo",
            Direction::Redo => "redo",
        }
    }

    /// What a conflicting step's actor should do next. The step is dropped from their history so that it can never
    /// wedge it: the next call reaches the step before.
    fn dropped_hint(self) -> &'static str {
        match self {
            Direction::Undo => {
                "that step is dropped from your undo history, so the next undo reaches the step before it; \
                 run an op to make the change again if you still want it"
            }
            Direction::Redo => {
                "that step is dropped from your redo history, so the next redo reaches the one before it; \
                 run an op to make the change again if you still want it"
            }
        }
    }
}

/// An edit that has been planned and is ready to apply.
pub(crate) struct Edit<'a> {
    pub(crate) op: &'static str,
    pub(crate) scope: Scope,
    pub(crate) args: Value,
    pub(crate) patch: &'a Patch,
    pub(crate) actor: Actor,
    pub(crate) cause: Option<Cause>,
    /// The open transaction to join, or `None` for a one-op transaction of its own.
    pub(crate) tx: Option<TxId>,
    pub(crate) at: Seconds,
}

pub(crate) struct State {
    doc: Box<dyn DocWrite>,
    log: Vec<OpRecord>,
    last_tx: u64,
    open: HashMap<TxId, OpenTx>,
    /// One entry per actor that has committed something. A handful at most, so a vector beats a map (and `Actor`
    /// has no `Hash` or `Ord`).
    histories: Vec<History>,
    outbox: Vec<Event>,
}

impl State {
    pub(crate) fn new(doc: Box<dyn DocWrite>) -> Self {
        Self { doc, log: Vec::new(), last_tx: 0, open: HashMap::new(), histories: Vec::new(), outbox: Vec::new() }
    }

    pub(crate) fn doc(&self) -> &dyn DocRead {
        &*self.doc
    }

    pub(crate) fn log(&self) -> &[OpRecord] {
        &self.log
    }

    /// Hands over the queued events, oldest first.
    pub(crate) fn take_outbox(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.outbox)
    }

    /// Opens a transaction owned by `actor`.
    pub(crate) fn begin(&mut self, label: &str, actor: &Actor) -> TxId {
        let tx = next_tx(&mut self.last_tx);
        self.open.insert(tx, OpenTx { owner: actor.clone(), step: Step::new(tx, label) });
        tx
    }

    /// Checks that `tx` is open and owned by `actor`, without changing anything.
    pub(crate) fn check_tx(&mut self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        Self::open_tx(&mut self.open, tx, actor).map(|_| ())
    }

    /// Applies a planned edit, logs it and queues its events.
    ///
    /// # Errors
    /// The transaction isn't open or isn't `actor`'s; or the patch no longer applies (`Conflict`). On error nothing
    /// has changed.
    pub(crate) fn apply_edit(&mut self, edit: Edit<'_>) -> Result<OpRecord, OpError> {
        let Edit { op, scope, args, patch, actor, cause, tx, at } = edit;
        // Borrowing the open transaction first checks it *before* the document changes, and lets us add to it after
        // without a lookup that could fail.
        let joined = tx.map(|id| Self::open_tx(&mut self.open, id, &actor)).transpose()?;
        let version = if patch.changes.is_empty() {
            self.doc.version()
        } else {
            self.doc.apply(patch).map_err(|e| error::apply_failed(op, e))?
        };

        let tx = joined.as_ref().map_or_else(|| next_tx(&mut self.last_tx), |open| open.step.tx);
        let record = OpRecord {
            seq: self.log.len() as u64 + 1,
            op: op.to_owned(),
            args,
            actor,
            cause,
            tx,
            at,
            patch: patch.clone(),
            version,
            on_behalf_of: None,
            approval: None,
        };
        self.log.push(record.clone());
        self.outbox.push(Event::OpApplied { record: record.clone() });

        if let Some(open) = joined {
            open.step.absorb(scope, patch);
        } else {
            let mut step = Step::new(tx, op);
            step.absorb(scope, patch);
            self.finish(&record.actor, step);
        }
        Ok(record)
    }

    /// Commits an open transaction as one undo step in its owner's history.
    pub(crate) fn commit(&mut self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.check_tx(tx, actor)?;
        let open = self.open.remove(&tx).ok_or_else(|| error::tx_not_open(tx))?;
        self.finish(actor, open.step);
        Ok(())
    }

    /// Reverts every change of an open transaction and closes it. If the revert conflicts, the transaction stays
    /// open so its owner can commit it instead.
    pub(crate) fn rollback(&mut self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let open = Self::open_tx(&mut self.open, tx, actor)?;
        let revert = open.step.patch.inverse();
        let reverted = if revert.changes.is_empty() { Ok(()) } else { self.doc.apply(&revert).map(|_| ()) };
        if let Err(e) = reverted {
            let what = format!("roll back transaction {}", tx.0);
            let hint =
                "the transaction is still open: commit it to keep its changes, or retry once the clash is resolved";
            let changed_by = last_changer(&self.log, error::patch_error_path(&e));
            return Err(error::revert_failed(&what, &e, changed_by, hint));
        }
        self.open.remove(&tx);
        let version = self.doc.version();
        self.outbox.push(Event::TxRolledBack { tx, actor: actor.clone(), version });
        Ok(())
    }

    pub(crate) fn undo(&mut self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.travel(Direction::Undo, actor, grants)
    }

    pub(crate) fn redo(&mut self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.travel(Direction::Redo, actor, grants)
    }

    /// Finds `tx` among the open transactions and checks it belongs to `actor`.
    fn open_tx<'a>(open: &'a mut HashMap<TxId, OpenTx>, tx: TxId, actor: &Actor) -> Result<&'a mut OpenTx, OpError> {
        let found = open.get_mut(&tx).ok_or_else(|| error::tx_not_open(tx))?;
        if found.owner == *actor { Ok(found) } else { Err(error::tx_not_yours(tx, &found.owner)) }
    }

    /// `actor`'s history, created on first use.
    fn history(&mut self, actor: &Actor) -> &mut History {
        let index = self.histories.iter().position(|h| h.actor == *actor).unwrap_or_else(|| {
            self.histories.push(History { actor: actor.clone(), undo: Vec::new(), redo: Vec::new() });
            self.histories.len() - 1
        });
        &mut self.histories[index]
    }

    /// Closes a transaction owned by `owner`. Only steps that changed the document become undoable, and only they
    /// invalidate the owner's redo: running a query must not throw away the user's redo history, and one actor's
    /// edits must not throw away another's.
    fn finish(&mut self, owner: &Actor, step: Step) {
        self.outbox.push(Event::TxCommitted { tx: step.tx, label: step.label.clone(), actor: owner.clone() });
        if step.changes_document() {
            let history = self.history(owner);
            history.redo.clear();
            history.undo.push(step);
        }
    }

    /// Moves `actor`'s newest step from one of its stacks to the other, applying its patch (inverted for undo).
    ///
    /// If `grants` can't cover the step, it stays where it was (the actor may retry with more). If others' later
    /// changes make it impossible to apply, it is *dropped*: a step that can never apply would otherwise block every
    /// step beneath it. Either way the document is unchanged.
    fn travel(&mut self, direction: Direction, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        let Some(history) = self.histories.iter_mut().find(|h| h.actor == *actor) else { return Ok(None) };
        let (from, to) = match direction {
            Direction::Undo => (&mut history.undo, &mut history.redo),
            Direction::Redo => (&mut history.redo, &mut history.undo),
        };
        let Some(step) = from.pop() else { return Ok(None) };
        let verb = direction.verb();
        if let Err(denied) = step.require(actor, grants, verb) {
            from.push(step);
            return Err(denied);
        }
        let patch = match direction {
            Direction::Undo => step.patch.inverse(),
            Direction::Redo => step.patch.clone(),
        };
        if let Err(e) = self.doc.apply(&patch) {
            let changed_by = last_changer(&self.log, error::patch_error_path(&e));
            return Err(error::revert_failed(
                &format!("{verb} {:?}", step.label),
                &e,
                changed_by,
                direction.dropped_hint(),
            ));
        }
        let (tx, label) = (step.tx, step.label.clone());
        to.push(step);
        // Undo is per actor for now, so the actor owns the step it moves; per-principal undo makes them differ.
        let (actor, owner, version) = (actor.clone(), actor.clone(), self.doc.version());
        self.outbox.push(match direction {
            Direction::Undo => Event::Undone { tx, label: label.clone(), actor, owner, version },
            Direction::Redo => Event::Redone { tx, label: label.clone(), actor, owner, version },
        });
        Ok(Some(label))
    }
}

/// Who most recently changed `path`, or something above or below it, according to the op log. Best effort: it names
/// the last *logged* op that touched the area, which is the one that made an inverse patch stop applying.
fn last_changer<'a>(log: &'a [OpRecord], path: &DocPath) -> Option<&'a Actor> {
    let touches = |change: &omuzaki_doc_api::Change| change.path.starts_with(path) || path.starts_with(&change.path);
    log.iter().rev().find(|record| record.patch.changes.iter().any(touches)).map(|record| &record.actor)
}

/// Allocates the next transaction id (ids start at 1). A free function so callers can use it while another field of
/// the state is borrowed.
fn next_tx(last: &mut u64) -> TxId {
    *last += 1;
    TxId(*last)
}
