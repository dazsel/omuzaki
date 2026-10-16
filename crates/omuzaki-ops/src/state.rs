//! The executor's mutable core: the document, the op log, open transactions and each actor's undo/redo stacks.
//!
//! Everything here runs under the executor's single lock and is synchronous. Events aren't sent from here: they're
//! queued in an outbox, which the executor drains after releasing the lock (see [`Bus::deliver`](crate::bus::Bus)).

use std::collections::{BTreeSet, HashMap};

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch, Value};
use omuzaki_ops_api::{
    Actor, ApprovalRef, Cause, Event, Grants, KeptTx, OpError, OpRecord, Scope, SessionId, SessionInfo, SessionTx,
    SessionUndo, TxId, Undo, checks,
};

use crate::approvals::Queue;
use crate::error;
use crate::sessions::Registry;

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
    /// The first op with effects outside the document (`Undo::Explicit`). Undo and redo can't revert those yet, so
    /// they refuse the step.
    explicit: Option<&'static str>,
}

impl Step {
    fn new(tx: TxId, label: &str) -> Self {
        Self { tx, label: label.to_owned(), patch: Patch::new(), scopes: BTreeSet::new(), explicit: None }
    }

    /// Adds one applied op, recording the scope [`checks::step_scope`] gives it.
    fn absorb(&mut self, op: &'static str, scope: Scope, undo: Undo, patch: &Patch) {
        self.scopes.extend(checks::step_scope(scope, undo, patch));
        if undo == Undo::Explicit && self.explicit.is_none() {
            self.explicit = Some(op);
        }
        self.patch.extend(patch.clone());
    }

    /// Whether the step belongs in history ([`checks::in_history`]).
    fn changes_document(&self) -> bool {
        checks::in_history(&self.patch, self.explicit.is_some())
    }

    fn require(&self, actor: &Actor, grants: &Grants, verb: &str) -> Result<(), OpError> {
        checks::require_step_scopes(self.scopes.iter().copied(), grants, actor, verb, &self.label)
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
    pub(crate) undo: Undo,
    pub(crate) args: Value,
    pub(crate) patch: &'a Patch,
    pub(crate) actor: Actor,
    pub(crate) cause: Option<Cause>,
    /// The open transaction to join, or `None` for a one-op transaction of its own.
    pub(crate) tx: Option<TxId>,
    pub(crate) at: Seconds,
    /// The approval that allowed the run, stamped on its record.
    pub(crate) approval: Option<ApprovalRef>,
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
    /// The agent sessions and their principals. Under the same lock as the rest, so a pause and the check that honours
    /// it cannot interleave.
    pub(crate) sessions: Registry,
    /// The approvals waiting for a person. Under the same lock, so each is resolved or withdrawn exactly once.
    pub(crate) approvals: Queue,
}

impl State {
    pub(crate) fn new(doc: Box<dyn DocWrite>) -> Self {
        Self {
            doc,
            log: Vec::new(),
            last_tx: 0,
            open: HashMap::new(),
            histories: Vec::new(),
            outbox: Vec::new(),
            sessions: Registry::new(),
            approvals: Queue::new(),
        }
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

    /// Queues an event that no edit produced (a job changed state), behind everything queued so far.
    pub(crate) fn queue_event(&mut self, event: Event) {
        self.outbox.push(event);
    }

    /// One session as the panel shows it: the registry's record with the effective grants, the open transaction and
    /// the pending approvals filled in. With several open transactions, the oldest is the one shown.
    pub(crate) fn session_info(&self, session: &SessionInfo) -> SessionInfo {
        let open_tx = self
            .open
            .iter()
            .filter(|(_, open)| open.owner == session.actor)
            .min_by_key(|(tx, _)| **tx)
            .map(|(tx, open)| SessionTx { tx: *tx, label: open.step.label.clone() });
        let pending = self.approvals.of_session(session.id);
        SessionInfo { grants: self.sessions.effective(session), open_tx, pending, ..session.clone() }
    }

    /// The snapshot of session `id`, if there is one.
    pub(crate) fn session(&self, id: SessionId) -> Option<SessionInfo> {
        self.sessions.find(id).map(|s| self.session_info(s))
    }

    /// Reserves a transaction id that is never opened, for a `begin` that was refused: `Runner::begin` cannot return an
    /// error, so the id it hands back fails closed on every later use.
    pub(crate) fn reserve_tx(&mut self) -> TxId {
        next_tx(&mut self.last_tx)
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
        let Edit { op, scope, undo, args, patch, actor, cause, tx, at, approval } = edit;
        // Borrowing the open transaction first checks it *before* the document changes, and lets us add to it after
        // without a lookup that could fail.
        let joined = tx.map(|id| Self::open_tx(&mut self.open, id, &actor)).transpose()?;
        let version = checks::apply_patch(&mut *self.doc, patch).map_err(|e| error::apply_failed(op, e))?;

        let tx = joined.as_ref().map_or_else(|| next_tx(&mut self.last_tx), |open| open.step.tx);
        let principal = self.sessions.principal_of(&actor).cloned();
        let mut record =
            OpRecord::new(self.log.len() as u64 + 1, op, args, actor, tx, at, patch.clone(), version).with_cause(cause);
        if let Some(person) = principal {
            record = record.with_on_behalf_of(person);
        }
        if let Some(approval) = approval {
            record = record.with_approval(approval);
        }
        self.log.push(record.clone());
        self.outbox.push(Event::op_applied(record.clone()));

        if let Some(open) = joined {
            open.step.absorb(op, scope, undo, patch);
        } else {
            let mut step = Step::new(tx, op);
            step.absorb(op, scope, undo, patch);
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
        let hint = "the transaction is still open: commit it to keep its changes, or retry once the clash is resolved";
        self.revert(tx, &revert, hint)?;
        self.open.remove(&tx);
        let version = self.doc.version();
        self.outbox.push(Event::tx_rolled_back(tx, actor.clone(), version));
        Ok(())
    }

    /// Applies `revert`, the inverse of transaction `tx`'s changes, or says why it can't: someone changed the same
    /// paths since. The refusal names the path and who changed it, never a value.
    fn revert(&mut self, tx: TxId, revert: &Patch, hint: &str) -> Result<(), OpError> {
        if revert.changes.is_empty() {
            return Ok(());
        }
        self.doc.apply(revert).map(|_| ()).map_err(|e| {
            let changed_by = last_changer(&self.log, error::patch_error_path(&e));
            error::revert_failed(&format!("roll back transaction {}", tx.0), &e, changed_by, hint)
        })
    }

    /// Abandons every open transaction `actor` owns, newest first (design §4.2): each is rolled back, or, when the
    /// revert conflicts, committed as it is with " (unfinished)" added to its label, so that nobody is left with a
    /// transaction no one can close. A conflict never stops the rest, and other actors' transactions are untouched.
    /// Queues `TxRolledBack` or `TxCommitted` for each. Returns the rolled-back ids and the kept transactions.
    pub(crate) fn abandon(&mut self, actor: &Actor) -> (Vec<TxId>, Vec<KeptTx>) {
        let mut owned: Vec<(TxId, OpenTx)> = self.open.extract_if(|_, open| open.owner == *actor).collect();
        owned.sort_unstable_by(|(a, _), (b, _)| b.cmp(a));
        let (mut abandoned, mut kept) = (Vec::new(), Vec::new());
        for (tx, mut open) in owned {
            let hint = "it is kept as an unfinished step in the agent's history; undoing the session tries again";
            match self.revert(tx, &open.step.patch.inverse(), hint) {
                Ok(()) => {
                    let version = self.doc.version();
                    self.outbox.push(Event::tx_rolled_back(tx, actor.clone(), version));
                    abandoned.push(tx);
                }
                Err(error) => {
                    open.step.label.push_str(" (unfinished)");
                    kept.push(KeptTx { tx, label: open.step.label.clone(), error });
                    self.finish(actor, open.step);
                }
            }
        }
        (abandoned, kept)
    }

    /// Reverts `agent`'s committed steps newest first, as one step in `by`'s history (design §6, undo session).
    ///
    /// A step whose inverse conflicts is skipped and reported, naming who changed the path, like `Runner::undo`; an
    /// older step that depends on it then conflicts too. A step holding an [`Undo::Explicit`] op is not reverted: its
    /// effect outside the document has no inverse here, so it is listed in `outside`. The agent's undo and redo
    /// stacks are cleared. The reverts become one step labelled "Undo <client>'s session (N steps)", so `by`'s undo
    /// brings the agent's work back; `SessionUndone` is queued, then the step's `TxCommitted`. `grants` are `by`'s:
    /// a step whose scopes they lack is skipped. `abandoned` (the open transactions already abandoned) goes into the
    /// result as it is.
    pub(crate) fn undo_session(
        &mut self,
        session: SessionId,
        agent: &Actor,
        by: &Actor,
        grants: &Grants,
        client: &str,
        abandoned: Vec<TxId>,
    ) -> SessionUndo {
        let undo = match self.histories.iter_mut().find(|h| h.actor == *agent) {
            Some(history) => {
                history.redo.clear();
                std::mem::take(&mut history.undo)
            }
            None => Vec::new(),
        };
        let (mut reverted, mut skipped, mut outside) = (Vec::new(), Vec::new(), Vec::new());
        let mut combined = Patch::new();
        let mut scopes = BTreeSet::new();
        for step in undo.into_iter().rev() {
            if step.explicit.is_some() {
                outside.push(step.tx);
                continue;
            }
            if let Err(error) = step.require(by, grants, "undo") {
                skipped.push(KeptTx { tx: step.tx, label: step.label, error });
                continue;
            }
            let inverse = step.patch.inverse();
            match self.doc.apply(&inverse) {
                Ok(_) => {
                    reverted.push(step.tx);
                    combined.extend(inverse);
                    scopes.extend(step.scopes);
                }
                Err(e) => {
                    let changed_by = last_changer(&self.log, error::patch_error_path(&e));
                    let hint = "that step stays applied; the steps before it may depend on it";
                    let error = error::revert_failed(&format!("undo {:?}", step.label), &e, changed_by, hint);
                    skipped.push(KeptTx { tx: step.tx, label: step.label, error });
                }
            }
        }
        let step = (!reverted.is_empty()).then(|| {
            let tx = next_tx(&mut self.last_tx);
            let noun = if reverted.len() == 1 { "step" } else { "steps" };
            let label = format!("Undo {client}'s session ({} {noun})", reverted.len());
            Step { tx, label, patch: combined, scopes, explicit: None }
        });
        let result = SessionUndo { step: step.as_ref().map(|s| s.tx), reverted, skipped, abandoned, outside };
        self.outbox.push(Event::session_undone(session, by.clone(), result.clone()));
        if let Some(step) = step {
            self.finish(by, step);
        }
        result
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
        self.outbox.push(Event::tx_committed(step.tx, step.label.clone(), owner.clone()));
        if step.changes_document() {
            self.sessions.count_step(owner);
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
        // Refused and dropped, not pushed back: the document is untouched, and the next call reaches the step before.
        if let Some(op) = step.explicit {
            return Err(checks::explicit_refused(verb, &step.label, op));
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
            Direction::Undo => Event::undone(tx, label.clone(), actor, owner, version),
            Direction::Redo => Event::redone(tx, label.clone(), actor, owner, version),
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

#[cfg(test)]
mod tests {
    use omuzaki_doc_mock::MemDoc;
    use omuzaki_ops_api::ErrorCode;
    use serde_json::json;

    use super::*;

    /// A step with an `Explicit` op that changed `/a`, put on `actor`'s redo stack directly. Undo never leaves such a
    /// step there, so this stands for a history restored from another build.
    #[test]
    fn redo_refuses_an_explicit_step_and_drops_it_without_touching_the_document() {
        let actor = Actor::Person { name: "alice".into() };
        let mut state = State::new(Box::new(MemDoc::from_value(json!({}))));
        let path = DocPath::parse("a").unwrap();
        let change = Patch::new().with(path.clone(), None, Some(json!(1)));
        let mut step = Step::new(TxId(1), "explicit op");
        step.absorb("test.explicit", Scope::EditCharacter, Undo::Explicit, &change);
        state.history(&actor).redo.push(step);

        let e = state.redo(&actor, &Grants::person()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert!(e.message.contains("test.explicit"), "{e}");
        assert_eq!(state.doc().get(&path), None, "the document is untouched");
        assert_eq!(state.redo(&actor, &Grants::person()).unwrap(), None, "the refused step is gone");
    }
}
