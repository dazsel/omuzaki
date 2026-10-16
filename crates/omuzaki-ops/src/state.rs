//! The executor's mutable core: the document, the op log, open transactions, every actor's history, the sessions and
//! approvals, the index of last changes and the journal hand-off.
//!
//! Everything here runs under the executor's single lock and is synchronous. Events aren't sent from here: they're
//! queued in an outbox, which the executor drains after releasing the lock (see [`Bus::deliver`](crate::bus::Bus)).
//!
//! **Journal order is history order.** Each change appends its entry here, under the lock, right after it succeeds,
//! and every document change is exactly one entry with the patch that was applied: an op, a rollback, an undo, a redo,
//! or undo-session's combined step. So replaying the entries reproduces the document version by version. Removals from
//! a stack that replay could not work out by itself (a step past the undo depth, another actor's redo cleared by a
//! commit of the same person) are journalled as `Dropped`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, Value};
use omuzaki_history_api::{HISTORY_EXPORT_FORMAT, HistoryEntry, HistoryExport, Stack, WallClock, WallTime};
use omuzaki_ops_api::{
    Actor, ApprovalRef, Cause, Effect, Event, JobId, OpError, OpRecord, RevertConflict, Scope, SessionId, SessionInfo,
    SessionTx, StopStep, TxId, Undo, checks,
};
use serde_json::json;

use crate::approvals::Queue;
use crate::error;
use crate::guard::Index;
use crate::history::{Direction, Draft, Driven, Histories, HistoryLimits, OpenTx, Step};
use crate::journal::Journal;
use crate::sessions::Registry;

/// The op name of the record an undo-session's combined step is logged and journalled as. It is not a registered op:
/// replay applies patches, never ops, and the History panel reads its label.
pub(crate) const SESSION_UNDO_OP: &str = "session.undo";

/// How long an actor's history is kept without the actor acting (`journal.md` §9).
const STALE_AFTER_MS: u64 = 30 * 24 * 3600 * 1000;

/// An edit that has been planned and is ready to apply.
pub(crate) struct Edit<'a> {
    pub(crate) op: &'static str,
    /// The op's summary: a one-op step's label in undo menus (#1662).
    pub(crate) summary: &'static str,
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
    /// What an `Undo::Explicit` op did outside the document.
    pub(crate) effect: Option<Effect>,
    /// The driver job making this edit as an inner call: its event waits for the transaction's commit.
    pub(crate) driver: Option<JobId>,
}

pub(crate) struct State {
    doc: Box<dyn DocWrite>,
    /// The newest records, at most `log_cap`; older ones are in the journal's archive.
    log: VecDeque<Arc<OpRecord>>,
    next_seq: u64,
    log_cap: usize,
    last_tx: u64,
    open: HashMap<TxId, OpenTx>,
    pub(crate) histories: Histories,
    outbox: Vec<Event>,
    /// The agent sessions and their principals. Under the same lock as the rest, so a pause and the check that honours
    /// it cannot interleave.
    pub(crate) sessions: Registry,
    /// The approvals waiting for a person. Under the same lock, so each is resolved or withdrawn exactly once.
    pub(crate) approvals: Queue,
    /// Who last changed each path: the stale-write guard and the names in conflict messages.
    pub(crate) changes: Index,
    pub(crate) journal: Journal,
    wall: Option<Arc<dyn WallClock>>,
}

impl State {
    pub(crate) fn new(doc: Box<dyn DocWrite>) -> Self {
        let limits = HistoryLimits::default();
        Self {
            doc,
            log: VecDeque::new(),
            next_seq: 1,
            log_cap: limits.log_records,
            last_tx: 0,
            open: HashMap::new(),
            histories: Histories::new(limits.undo_depth),
            outbox: Vec::new(),
            sessions: Registry::new(),
            approvals: Queue::new(),
            changes: Index::default(),
            journal: Journal::default(),
            wall: None,
        }
    }

    pub(crate) fn set_limits(&mut self, limits: HistoryLimits) {
        self.log_cap = limits.log_records;
        self.histories.set_depth(limits.undo_depth);
    }

    pub(crate) fn set_journal(&mut self, journal: Journal) {
        self.journal = journal;
    }

    pub(crate) fn set_wall(&mut self, wall: Arc<dyn WallClock>) {
        self.wall = Some(wall);
    }

    pub(crate) fn doc(&self) -> &dyn DocRead {
        &*self.doc
    }

    /// Up to `limit` kept records with `seq` greater than `after_seq`, oldest first.
    pub(crate) fn log_after(&self, after_seq: u64, limit: usize) -> Vec<OpRecord> {
        let first = self.log.front().map_or(self.next_seq, |record| record.seq);
        // Sequence numbers are contiguous in the kept window, so the records after `after_seq` start at this index.
        let start = usize::try_from(after_seq.saturating_add(1).saturating_sub(first)).unwrap_or(usize::MAX);
        self.log.iter().skip(start).take(limit).map(|record| OpRecord::clone(record)).collect()
    }

    /// Hands over the queued events, oldest first. The state is whole here (no change is half done), so this is also
    /// where the journal gets the `Mark` it asked for.
    pub(crate) fn take_outbox(&mut self) -> Vec<Event> {
        self.mark_if_wanted();
        std::mem::take(&mut self.outbox)
    }

    /// How many events are queued: replay forgets those it queues ([`forget_events_after`](Self::forget_events_after)).
    pub(crate) fn queued(&self) -> usize {
        self.outbox.len()
    }

    /// Drops the events queued after the first `kept`.
    pub(crate) fn forget_events_after(&mut self, kept: usize) {
        self.outbox.truncate(kept);
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
            .map(|(tx, open)| SessionTx { tx: *tx, label: open.label.clone() });
        let pending = self.approvals.of_session(session.id);
        SessionInfo { grants: self.sessions.effective(session), open_tx, pending, ..session.clone() }
    }

    /// The snapshot of session `id`, if there is one.
    pub(crate) fn session(&self, id: SessionId) -> Option<SessionInfo> {
        self.sessions.find(id).map(|s| self.session_info(s))
    }

    /// Reserves a transaction id that is never opened: for a `begin` that was refused (`Runner::begin` cannot return
    /// an error, so the id it hands back fails closed on every later use), and for undo-session's step.
    pub(crate) fn reserve_tx(&mut self) -> TxId {
        next_tx(&mut self.last_tx)
    }

    /// Opens a transaction owned by `actor`.
    pub(crate) fn begin(&mut self, label: &str, actor: &Actor, at: Seconds) -> TxId {
        let tx = next_tx(&mut self.last_tx);
        self.open.insert(tx, OpenTx::new(actor.clone(), label.to_owned(), Draft::default()));
        self.journal.append(|| HistoryEntry::Begin { tx, actor: actor.clone(), label: label.to_owned(), at });
        tx
    }

    /// Checks that `tx` is open and owned by `actor`, without changing anything.
    pub(crate) fn check_tx(&mut self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        Self::open_tx(&mut self.open, tx, actor).map(|_| ())
    }

    /// Applies a planned edit, logs it, journals it and queues its events.
    ///
    /// # Errors
    /// The transaction isn't open or isn't `actor`'s; or the patch no longer applies (`Conflict`). On error nothing
    /// has changed.
    pub(crate) fn apply_edit(&mut self, edit: Edit<'_>) -> Result<OpRecord, OpError> {
        let Edit { op, summary, scope, undo, args, patch, actor, cause, tx, at, approval, effect, driver } = edit;
        // Borrowing the open transaction first checks it *before* the document changes.
        if let Some(id) = tx {
            Self::open_tx(&mut self.open, id, &actor)?;
        }
        let version = checks::apply_patch(&mut *self.doc, patch).map_err(|e| error::apply_failed(op, e))?;
        let tx_id = tx.unwrap_or_else(|| next_tx(&mut self.last_tx));
        let mut record =
            OpRecord::new(self.next_seq, op, args, actor, tx_id, at, patch.clone(), version).with_cause(cause);
        self.next_seq += 1;
        if let Some(person) = self.sessions.principal_of(&record.actor) {
            record = record.with_on_behalf_of(person.clone());
        }
        if let Some(approval) = approval {
            record = record.with_approval(approval);
        }
        if let Some(effect) = &effect {
            record = record.with_effect(effect.clone());
        }
        self.changes.record(patch, &record.actor, version);
        let record = self.log_record(record, tx.is_none(), driver.is_some());

        match tx.and_then(|id| self.open.get_mut(&id)) {
            Some(open) => {
                let (changes, effects) = (open.draft.patch.changes.len(), open.draft.effects.len());
                open.draft.absorb(scope, undo, patch, effect.as_ref());
                if let Some(job) = driver {
                    let changes = changes..open.draft.patch.changes.len();
                    let effects = effects..open.draft.effects.len();
                    open.driven.push(Driven { job, seq: record.seq, changes, effects });
                    open.held.push(Event::op_applied(record.clone()));
                }
            }
            None => {
                let mut draft = Draft::default();
                draft.absorb(scope, undo, patch, effect.as_ref());
                self.finish(&record.actor, tx_id, summary.to_owned(), draft);
            }
        }
        Ok(record)
    }

    /// Appends `record` to the log (dropping the oldest past the cap), queues its event (unless a driver's
    /// transaction `holds` it until its commit) and journals it (`commits`: it is a one-op transaction). Returns it.
    fn log_record(&mut self, record: OpRecord, commits: bool, holds: bool) -> OpRecord {
        let shared = Arc::new(record);
        self.log.push_back(shared.clone());
        while self.log.len() > self.log_cap {
            self.log.pop_front();
        }
        if !holds {
            self.outbox.push(Event::op_applied(OpRecord::clone(&shared)));
        }
        self.journal.append(|| HistoryEntry::Op { record: shared.clone(), commits });
        OpRecord::clone(&shared)
    }

    /// Commits an open transaction as one undo step in its owner's history.
    pub(crate) fn commit(&mut self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.check_tx(tx, actor)?;
        let open = self.open.remove(&tx).ok_or_else(|| error::tx_not_open(tx))?;
        self.journal.append(|| HistoryEntry::Commit { tx });
        // Driver jobs' inner edits reach subscribers now, in order, before the commit.
        self.outbox.extend(open.held);
        self.finish(actor, tx, open.label, open.draft);
        Ok(())
    }

    /// What rolling back open transaction `tx`, owned by `actor`, has to undo: its effects in apply order, and its
    /// revert patch.
    pub(crate) fn open_undo(&mut self, tx: TxId, actor: &Actor) -> Result<(Vec<Effect>, Patch), OpError> {
        Self::open_tx(&mut self.open, tx, actor).map(|open| (open.draft.effects.clone(), open.draft.patch.inverse()))
    }

    /// Reverts every change of an open transaction and closes it. If the revert conflicts, the transaction stays
    /// open so its owner can commit it instead.
    pub(crate) fn rollback(&mut self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        let open = Self::open_tx(&mut self.open, tx, actor)?;
        let revert = open.draft.patch.inverse();
        let hint = "the transaction is still open: commit it to keep its changes, or retry once the clash is resolved";
        self.revert(tx, &revert, actor, hint)?;
        self.open.remove(&tx);
        let version = self.doc.version();
        self.journal.append(|| HistoryEntry::Rollback { tx, patch: revert });
        self.outbox.push(Event::tx_rolled_back(tx, actor.clone(), version));
        Ok(())
    }

    /// Applies `revert`, the inverse of transaction `tx`'s changes, as `by`, or says why it can't: someone changed the
    /// same paths since. The refusal names the path and who changed it, never a value.
    fn revert(&mut self, tx: TxId, revert: &Patch, by: &Actor, hint: &str) -> Result<(), OpError> {
        if revert.changes.is_empty() {
            return Ok(());
        }
        match self.doc.apply(revert) {
            Ok(version) => {
                self.changes.record(revert, by, version);
                Ok(())
            }
            Err(e) => Err(self.conflict(&format!("roll back transaction {}", tx.0), &e, hint)),
        }
    }

    /// A conflict refusal for `what`, naming the path and who changed it last (or saying the change is older than
    /// what this executor knows).
    pub(crate) fn conflict(&self, what: &str, e: &PatchError, hint: &str) -> OpError {
        let path = error::patch_error_path(e);
        error::revert_failed(what, e, self.changes.newest(path, None).map(|c| c.by), hint)
    }

    /// The changes and effects driver job `job` added to open transaction `tx`, in apply order.
    pub(crate) fn driven_by(&self, tx: TxId, job: JobId) -> Option<(Patch, Vec<Effect>)> {
        let open = self.open.get(&tx)?;
        let mut patch = Patch::new();
        let mut effects = Vec::new();
        for edit in open.driven.iter().filter(|d| d.job == job) {
            patch.changes.extend(open.draft.patch.changes[edit.changes.clone()].iter().cloned());
            effects.extend(open.draft.effects[edit.effects.clone()].iter().cloned());
        }
        Some((patch, effects))
    }

    /// Why a driver's `edits` cannot be reverted: every path whose value is no longer what the job left there (with
    /// who changed it since), and every other path the job edited, which a revert would have kept too.
    pub(crate) fn unreverted(&self, edits: &Patch) -> (Vec<RevertConflict>, Vec<DocPath>) {
        // The job's last change to each path is what it left there.
        let mut left: Vec<(&DocPath, Option<&Value>)> = Vec::new();
        for change in &edits.changes {
            match left.iter_mut().find(|(path, _)| **path == change.path) {
                Some(slot) => slot.1 = change.after.as_ref(),
                None => left.push((&change.path, change.after.as_ref())),
            }
        }
        let (mut conflicts, mut retained) = (Vec::new(), Vec::new());
        for (path, job_value) in left {
            let current = self.doc.get(path);
            if current == job_value {
                retained.push(path.clone());
            } else {
                let actor = self.changes.newest(path, None).map_or(Actor::System, |last| last.by.clone());
                let job_value = job_value.cloned().unwrap_or(Value::Null);
                conflicts.push(RevertConflict { path: path.clone(), job_value, current: current.cloned(), actor });
            }
        }
        (conflicts, retained)
    }

    /// Applies `revert`, already tried, which undoes driver job `job`'s own edits in open transaction `tx`, as `by`;
    /// takes those edits (and their held events) out of the transaction; and journals one `TxRevert` with the
    /// transaction as it now stands (ADR 0036 decision 10). The scopes the job's ops added stay on the transaction:
    /// undoing it then asks for a scope it may no longer need, never one too few.
    ///
    /// # Errors
    /// `NotFound` if `tx` isn't open; `Conflict` if the revert does not apply after all (nothing changed).
    pub(crate) fn revert_driven(&mut self, tx: TxId, job: JobId, revert: &Patch, by: &Actor) -> Result<(), OpError> {
        if !self.open.contains_key(&tx) {
            return Err(error::tx_not_open(tx));
        }
        let hint = "the job's edits stay in the transaction";
        let version = match checks::apply_patch(&mut *self.doc, revert) {
            Ok(version) => version,
            Err(e) => return Err(self.conflict(&format!("revert job {}'s edits", job.0), &e, hint)),
        };
        self.changes.record(revert, by, version);
        let open = self.open.get_mut(&tx).ok_or_else(|| error::tx_not_open(tx))?;
        let mine: Vec<Driven> = open.driven.extract_if(.., |d| d.job == job).collect();
        // Take the job's ranges out from the back, so the earlier ones stay where they are, and move every remaining
        // driver edit down past what was taken before it.
        for edit in mine.iter().rev() {
            open.draft.patch.changes.drain(edit.changes.clone());
            open.draft.effects.drain(edit.effects.clone());
        }
        for other in &mut open.driven {
            let changes_before: usize =
                mine.iter().filter(|m| m.changes.end <= other.changes.start).map(|m| m.changes.len()).sum();
            let effects_before: usize =
                mine.iter().filter(|m| m.effects.end <= other.effects.start).map(|m| m.effects.len()).sum();
            other.changes = other.changes.start - changes_before..other.changes.end - changes_before;
            other.effects = other.effects.start - effects_before..other.effects.end - effects_before;
        }
        let seqs: Vec<u64> = mine.iter().map(|m| m.seq).collect();
        open.held.retain(|event| !matches!(event, Event::OpApplied { record, .. } if seqs.contains(&record.seq)));
        let remaining = open.export(tx);
        self.journal.append(|| HistoryEntry::TxRevert { tx, seqs, patch: revert.clone(), remaining });
        Ok(())
    }

    /// The open transactions `actor` owns, newest first: ending its session abandons them in this order.
    pub(crate) fn owned_open(&self, actor: &Actor) -> Vec<TxId> {
        let mut owned: Vec<TxId> =
            self.open.iter().filter(|(_, open)| open.owner == *actor).map(|(tx, _)| *tx).collect();
        owned.sort_unstable_by(|a, b| b.cmp(a));
        owned
    }

    /// Commits open transaction `tx` as it is, with `suffix` added to its label (" (unfinished)" when an end could
    /// not revert it). Returns its new label.
    pub(crate) fn commit_as_is(&mut self, tx: TxId, actor: &Actor, suffix: &str) -> Result<String, OpError> {
        let open = Self::open_tx(&mut self.open, tx, actor)?;
        open.label.push_str(suffix);
        let label = open.label.clone();
        self.commit(tx, actor)?;
        Ok(label)
    }

    /// Closes `actor`'s open transaction `tx` for a person's Stop (agent-runtime D4): committed as it stands with
    /// " (stopped)" added to its label, or, left empty, closed with no step.
    pub(crate) fn stop_tx(&mut self, tx: TxId, actor: &Actor) -> Result<StopStep, OpError> {
        let open = Self::open_tx(&mut self.open, tx, actor)?;
        if !open.draft.changes_anything() {
            self.commit(tx, actor)?;
            return Ok(StopStep::None);
        }
        self.commit_as_is(tx, actor, " (stopped)")?;
        Ok(StopStep::Committed(tx))
    }

    /// The history to undo from (or redo from) for `actor`: its own, or for a person the newest step among their own
    /// history and those of their agents' sessions (design §5.4; automation sessions are standing rules, not
    /// assistants, so they stay out, `automation.md` D7).
    pub(crate) fn pick(&self, direction: Direction, actor: &Actor) -> Option<usize> {
        match actor {
            Actor::Person { .. } => {
                let mut group = vec![actor.clone()];
                group.extend(
                    self.sessions
                        .all()
                        .filter(|s| self.principal_key(&s.actor) == Some(actor))
                        .map(|s| s.actor.clone()),
                );
                self.histories.pick(direction, &group)
            }
            _ => self.histories.pick(direction, std::slice::from_ref(actor)),
        }
    }

    /// The person whose undo covers `actor`'s steps: the person themselves, or the principal of a session that is not
    /// an automation. `None` for the system, plugins without a session and automations.
    fn principal_key<'a>(&'a self, actor: &'a Actor) -> Option<&'a Actor> {
        match actor {
            Actor::Person { .. } => Some(actor),
            _ => self
                .sessions
                .of_actor(actor)
                .filter(|s| s.transport != omuzaki_ops_api::Transport::Automation)
                .map(|s| &s.on_behalf_of),
        }
    }

    /// The owner of history `index`.
    pub(crate) fn owner(&self, index: usize) -> &Actor {
        &self.histories.get(index).actor
    }

    /// Drops `step`, taken from history `index`, after a conflict: journalled, so history can't wedge after a restart
    /// either.
    pub(crate) fn drop_step(&mut self, index: usize, direction: Direction, step: &Step) {
        let actor = self.histories.get(index).actor.clone();
        let stack = stack_of(direction);
        self.journal.append(|| HistoryEntry::Dropped { actor, tx: step.tx, stack });
    }

    /// Moves `step`, taken from history `index`, in `direction` as `doer`: applies its patch (inverted for undo),
    /// journals it, queues the event and lands it on the other stack. Returns its label.
    ///
    /// # Errors
    /// `Conflict` when others' later changes make the patch impossible to apply: the step is dropped (no-wedge rule)
    /// and the document is unchanged.
    pub(crate) fn move_step(
        &mut self,
        index: usize,
        direction: Direction,
        step: Step,
        doer: &Actor,
    ) -> Result<String, OpError> {
        let patch = match direction {
            Direction::Undo => step.patch.inverse(),
            Direction::Redo => Patch::clone(&step.patch),
        };
        let version = match self.doc.apply(&patch) {
            Ok(version) => version,
            Err(e) => {
                self.drop_step(index, direction, &step);
                return Err(self.conflict(
                    &format!("{} {:?}", direction.verb(), step.label),
                    &e,
                    direction.dropped_hint(),
                ));
            }
        };
        self.changes.record(&patch, doer, version);
        let owner = self.histories.get(index).actor.clone();
        let (tx, label) = (step.tx, step.label.clone());
        self.journal.append(|| match direction {
            Direction::Undo => HistoryEntry::Undo { actor: owner.clone(), tx, patch },
            Direction::Redo => HistoryEntry::Redo { actor: owner.clone(), tx, patch },
        });
        self.outbox.push(match direction {
            Direction::Undo => Event::undone(tx, label.clone(), doer.clone(), owner.clone(), version),
            Direction::Redo => Event::redone(tx, label.clone(), doer.clone(), owner.clone(), version),
        });
        self.histories.land(index, direction, step);
        self.seen(doer);
        Ok(label)
    }

    /// Takes `actor`'s whole history off its stacks, for undo-session: the undo steps newest first. The redo steps
    /// are dropped. Each removal is journalled as `Dropped`, so replay clears the same stacks.
    pub(crate) fn take_history(&mut self, actor: &Actor) -> Vec<Step> {
        let Some(index) = self.histories.find(actor) else { return Vec::new() };
        let history = self.histories.get_mut(index);
        let undo: Vec<Step> = std::mem::take(&mut history.undo).into_iter().rev().collect();
        let redo = std::mem::take(&mut history.redo);
        for (step, stack) in undo.iter().map(|s| (s, Stack::Undo)).chain(redo.iter().map(|s| (s, Stack::Redo))) {
            self.journal.append(|| HistoryEntry::Dropped { actor: actor.clone(), tx: step.tx, stack });
        }
        undo
    }

    /// Applies an undo-session's combined revert as one change: one record (op [`SESSION_UNDO_OP`]), one journalled
    /// transaction `tx` (reserved by the caller, so `SessionUndone` can name it first) and one step in `by`'s history,
    /// so `by`'s undo brings the agent's work back.
    ///
    /// # Errors
    /// `Internal` if the revert, already tried on a scratch copy, does not apply after all; nothing has changed then.
    pub(crate) fn commit_combined(
        &mut self,
        session: SessionId,
        tx: TxId,
        by: &Actor,
        label: String,
        draft: Draft,
        at: Seconds,
    ) -> Result<(), OpError> {
        let version = checks::apply_patch(&mut *self.doc, &draft.patch)
            .map_err(|e| OpError::internal(format!("undo of session {} did not apply as tried: {e}", session.0)))?;
        self.changes.record(&draft.patch, by, version);
        self.journal.append(|| HistoryEntry::Begin { tx, actor: by.clone(), label: label.clone(), at });
        let args = json!({ "session": session.0 });
        let record =
            OpRecord::new(self.next_seq, SESSION_UNDO_OP, args, by.clone(), tx, at, draft.patch.clone(), version);
        self.next_seq += 1;
        self.log_record(record, false, false);
        self.journal.append(|| HistoryEntry::Commit { tx });
        self.finish(by, tx, label, draft);
        Ok(())
    }

    /// Applies `patch` to the document as `by`, outside any op: replay's way in, which the journal already holds.
    pub(crate) fn apply_raw(&mut self, patch: &Patch, by: &Actor) -> Result<DocVersion, PatchError> {
        let version = checks::apply_patch(&mut *self.doc, patch)?;
        self.changes.record(patch, by, version);
        Ok(version)
    }

    /// Finds `tx` among the open transactions and checks it belongs to `actor`.
    fn open_tx<'a>(open: &'a mut HashMap<TxId, OpenTx>, tx: TxId, actor: &Actor) -> Result<&'a mut OpenTx, OpError> {
        let found = open.get_mut(&tx).ok_or_else(|| error::tx_not_open(tx))?;
        if found.owner == *actor { Ok(found) } else { Err(error::tx_not_yours(tx, &found.owner)) }
    }

    /// Closes a transaction owned by `owner`. Only steps that changed something become undoable, and only they
    /// invalidate redo: running a query must not throw away the user's redo history. A new step clears the redo of
    /// everyone its person's undo covers (the person and their agents, design §5.4), and nobody else's.
    pub(crate) fn finish(&mut self, owner: &Actor, tx: TxId, label: String, draft: Draft) {
        self.outbox.push(Event::tx_committed(tx, label.clone(), owner.clone()));
        if !draft.changes_anything() {
            return;
        }
        self.sessions.count_step(owner);
        self.clear_principal_redo(owner);
        let (fell, _own_redo) = self.histories.commit(owner, Step::committed(tx, label, draft));
        for step in fell {
            self.journal.append(|| HistoryEntry::Dropped { actor: owner.clone(), tx: step.tx, stack: Stack::Undo });
        }
        self.seen(owner);
    }

    /// Clears the redo stacks of the others whose steps `owner`'s person undoes, journalling each step dropped.
    fn clear_principal_redo(&mut self, owner: &Actor) {
        let Some(principal) = self.principal_key(owner) else { return };
        let others: Vec<Actor> = self
            .histories
            .iter()
            .filter(|h| !h.redo.is_empty() && h.actor != *owner && self.principal_key(&h.actor) == Some(principal))
            .map(|h| h.actor.clone())
            .collect();
        for actor in others {
            for step in self.histories.clear_redo(&actor) {
                self.journal.append(|| HistoryEntry::Dropped { actor: actor.clone(), tx: step.tx, stack: Stack::Redo });
            }
        }
    }

    /// Notes that `actor` acted now, in wall time, when the executor has a wall clock.
    fn seen(&mut self, actor: &Actor) {
        if let Some(wall) = &self.wall {
            let now = wall.now();
            self.histories.seen(actor, now);
        }
    }

    /// The whole history as data (#251): every actor's stacks, the open transactions and the next ids. Steps share
    /// their patches, so this copies pointers. Actors not seen for 30 days are left out (`journal.md` §9).
    pub(crate) fn history_export(&self) -> HistoryExport {
        let before = self.wall.as_ref().map(|wall| WallTime(wall.now().0.saturating_sub(STALE_AFTER_MS)));
        let mut export = HistoryExport::empty();
        export.format = HISTORY_EXPORT_FORMAT;
        export.actors = self.histories.export(before);
        let mut open: Vec<(&TxId, &OpenTx)> = self.open.iter().collect();
        open.sort_unstable_by_key(|(tx, _)| **tx);
        export.open = open.into_iter().map(|(tx, open)| open.export(*tx)).collect();
        export.next_tx = TxId(self.last_tx + 1);
        export.next_seq = self.next_seq;
        export
    }

    /// Replaces the history with `history` (#155): the stacks, the open transactions and the next ids, so ids never
    /// repeat. Actors not seen for 30 days before now are left out.
    pub(crate) fn restore(&mut self, history: &HistoryExport) {
        let before = self.wall.as_ref().map(|wall| WallTime(wall.now().0.saturating_sub(STALE_AFTER_MS)));
        self.histories.import(&history.actors, before);
        self.open = history.open.iter().map(|open| (open.tx, OpenTx::import(open))).collect();
        self.last_tx = history.next_tx.0.saturating_sub(1);
        self.next_seq = history.next_seq;
    }

    /// Appends a `Mark` with the history now.
    pub(crate) fn mark(&mut self) {
        if self.journal.on() {
            let history = Arc::new(self.history_export());
            self.journal.append(|| HistoryEntry::Mark { history });
        }
    }

    fn mark_if_wanted(&mut self) {
        if self.journal.wants_mark() {
            self.mark();
        }
    }

    /// The open transaction `tx`, whoever owns it: replay's handle on it.
    pub(crate) fn open_mut(&mut self, tx: TxId) -> Option<&mut OpenTx> {
        self.open.get_mut(&tx)
    }

    /// Opens transaction `tx` as replay found it in the journal. Ids continue after it.
    pub(crate) fn reopen(&mut self, tx: TxId, open: OpenTx) {
        self.last_tx = self.last_tx.max(tx.0);
        self.open.insert(tx, open);
    }

    /// Closes open transaction `tx` without touching the document: replay of a rollback whose patch it already
    /// applied, or of a commit.
    pub(crate) fn close(&mut self, tx: TxId) -> Option<OpenTx> {
        self.open.remove(&tx)
    }

    /// Notes `record`, read from the journal, as logged: ids continue after it.
    pub(crate) fn replayed(&mut self, record: Arc<OpRecord>) {
        self.next_seq = self.next_seq.max(record.seq + 1);
        self.last_tx = self.last_tx.max(record.tx.0);
        self.log.push_back(record);
        while self.log.len() > self.log_cap {
            self.log.pop_front();
        }
    }
}

/// The journal's name for a stack.
pub(crate) fn stack_of(direction: Direction) -> Stack {
    match direction {
        Direction::Undo => Stack::Undo,
        Direction::Redo => Stack::Redo,
    }
}

/// Allocates the next transaction id (ids start at 1). A free function so callers can use it while another field of
/// the state is borrowed.
fn next_tx(last: &mut u64) -> TxId {
    *last += 1;
    TxId(*last)
}
