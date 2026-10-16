//! Restoring the executor from a snapshot's history and replaying the journal after it (#155, design `journal.md`
//! §8.3 and §9): how undo and redo survive a restart, and how recovery rebuilds the executor after a crash.
//!
//! Replay is mechanical. Each entry's recorded patch is applied with its `before` check, and steps move exactly as in
//! live operation, through the same [`State`] code: nothing is planned, no event is sent, nothing is journalled again.
//! It stops at the first entry that doesn't apply, so a journal that disagrees with its base is never half-trusted.

use std::sync::Arc;

use omuzaki_core::time::Clock;
use omuzaki_doc_api::DocWrite;
use omuzaki_history_api::{HISTORY_EXPORT_FORMAT, HistoryEntry, HistoryExport, Lsn, Stack, WallClock};
use omuzaki_ops_api::{Actor, OpRecord, Scope, TxId, Undo};

use crate::ExecutorError;
use crate::executor::Executor;
use crate::history::{Direction, Draft, OpenTx};
use crate::state::State;
use crate::sync::lock;

/// What [`Executor::replay`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReplayReport {
    /// Entries replayed (marks and job entries included, which change no history).
    pub replayed: usize,
    /// The LSN of the last entry replayed; `Lsn(0)` if there were none.
    pub last: Lsn,
}

/// Why replay stopped: the entry at `lsn` does not apply to the executor as replay has left it. Every entry before it
/// was replayed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("replay stopped at LSN {}: {reason}", lsn.0)]
#[non_exhaustive]
pub struct ReplayError {
    /// The entry that did not apply.
    pub lsn: Lsn,
    /// Why.
    pub reason: String,
}

impl Executor {
    /// An executor over `doc` (the document as the snapshot holds it) with `history` (that snapshot's history export)
    /// restored: every actor's undo and redo stacks, the open transactions, and the next transaction id and op `seq`,
    /// so ids never repeat across sessions. Actors not seen for 30 days by `wall` are left out. Runs every op
    /// registered in the binary, as [`Executor::new`] does.
    ///
    /// # Errors
    /// [`ExecutorError::DuplicateOp`] as for [`Executor::new`]; [`ExecutorError::HistoryFormat`] for an export newer
    /// than this build reads.
    pub fn restore(
        doc: Box<dyn DocWrite>,
        clock: Arc<dyn Clock>,
        wall: Arc<dyn WallClock>,
        history: &HistoryExport,
    ) -> Result<Self, ExecutorError> {
        if history.format > HISTORY_EXPORT_FORMAT {
            return Err(ExecutorError::HistoryFormat { found: history.format, supported: HISTORY_EXPORT_FORMAT });
        }
        let exec = Self::new(doc, clock)?.with_wall_clock(wall);
        lock(&exec.env.state).restore(history);
        Ok(exec)
    }

    /// Replays journal `entries` (in LSN order) on top of what the executor holds: the document and the history, as
    /// [`restore`](Self::restore) left them, or as earlier replays did.
    ///
    /// # Errors
    /// [`ReplayError`] naming the first entry that does not apply (a `before` that doesn't match, an unknown
    /// transaction or step); the entries before it stay replayed.
    pub fn replay(&self, entries: &[(Lsn, HistoryEntry)]) -> Result<ReplayReport, ReplayError> {
        let mut state = lock(&self.env.state);
        // Replay changes nothing anyone else sees: no journal appends, no events.
        let journal = std::mem::take(&mut state.journal);
        let queued = state.queued();
        let mut report = ReplayReport { replayed: 0, last: Lsn(0) };
        let result = entries.iter().try_for_each(|(lsn, entry)| {
            self.replay_one(&mut state, entry).map_err(|reason| ReplayError { lsn: *lsn, reason })?;
            report.replayed += 1;
            report.last = *lsn;
            Ok(())
        });
        state.forget_events_after(queued);
        state.journal = journal;
        result.map(|()| report)
    }

    fn replay_one(&self, state: &mut State, entry: &HistoryEntry) -> Result<(), String> {
        match entry {
            HistoryEntry::Session { .. }
            | HistoryEntry::Mark { .. }
            | HistoryEntry::JobStarted { .. }
            | HistoryEntry::JobEnded { .. } => Ok(()),
            HistoryEntry::Begin { tx, actor, label, .. } => {
                if state.open_mut(*tx).is_some() {
                    return Err(format!("transaction {} is already open", tx.0));
                }
                state.reopen(*tx, OpenTx::new(actor.clone(), label.clone(), Draft::default()));
                Ok(())
            }
            HistoryEntry::Op { record, commits } => self.replay_op(state, record, *commits),
            HistoryEntry::Commit { tx } => {
                let open = state.close(*tx).ok_or_else(|| format!("transaction {} isn't open", tx.0))?;
                state.finish(&open.owner, *tx, open.label, open.draft);
                Ok(())
            }
            HistoryEntry::Rollback { tx, patch } => {
                let owner = state.open_mut(*tx).map(|open| open.owner.clone());
                let owner = owner.ok_or_else(|| format!("transaction {} isn't open", tx.0))?;
                state.apply_raw(patch, &owner).map_err(|e| e.to_string())?;
                state.close(*tx);
                Ok(())
            }
            HistoryEntry::Undo { actor, tx, patch } => replay_move(state, Direction::Undo, actor, *tx, patch),
            HistoryEntry::Redo { actor, tx, patch } => replay_move(state, Direction::Redo, actor, *tx, patch),
            HistoryEntry::Dropped { actor, tx, stack } => {
                // A step past the undo depth may already have fallen off when its commit was replayed.
                state.histories.remove(actor, direction_of(*stack), *tx);
                Ok(())
            }
            HistoryEntry::TxRevert { tx, patch, remaining, .. } => {
                let owner = state.open_mut(*tx).map(|open| open.owner.clone());
                let owner = owner.ok_or_else(|| format!("transaction {} isn't open", tx.0))?;
                state.apply_raw(patch, &owner).map_err(|e| e.to_string())?;
                state.reopen(*tx, OpenTx::import(remaining));
                Ok(())
            }
        }
    }

    /// Replays one applied op: its patch, then the transaction it joins or the one-op step it is.
    fn replay_op(&self, state: &mut State, record: &Arc<OpRecord>, commits: bool) -> Result<(), String> {
        if !commits && state.open_mut(record.tx).is_none() {
            return Err(format!("op {} joins transaction {}, which isn't open", record.seq, record.tx.0));
        }
        state.apply_raw(&record.patch, &record.actor).map_err(|e| e.to_string())?;
        // Scopes come from the registry, as live: an op this build no longer has adds none.
        let meta = self.env.ops.find(&record.op).ok().map(|registration| (registration.meta)());
        let (scope, undo) = meta.map_or((Scope::Read, Undo::Auto), |meta| (meta.scope, meta.undo));
        let scoped = meta.is_some();
        let absorb = |draft: &mut Draft| {
            if scoped {
                draft.absorb(scope, undo, &record.patch, record.effect.as_ref());
            } else {
                draft.patch.extend(record.patch.clone());
                draft.effects.extend(record.effect.clone());
            }
        };
        if commits {
            let mut draft = Draft::default();
            absorb(&mut draft);
            let label = meta.map_or_else(|| record.op.clone(), |meta| meta.summary.to_owned());
            state.finish(&record.actor, record.tx, label, draft);
        } else if let Some(open) = state.open_mut(record.tx) {
            absorb(&mut open.draft);
        }
        state.replayed(record.clone());
        Ok(())
    }
}

/// Replays an undo or a redo of `actor`'s step `tx`: it must be the next one on that stack.
fn replay_move(
    state: &mut State,
    direction: Direction,
    actor: &Actor,
    tx: TxId,
    patch: &omuzaki_doc_api::Patch,
) -> Result<(), String> {
    let index = state.histories.find(actor).ok_or_else(|| format!("{actor:?} has no history"))?;
    let next = state
        .histories
        .take(index, direction)
        .ok_or_else(|| format!("{actor:?} has nothing to {}", direction.verb()))?;
    if next.tx != tx {
        let found = next.tx;
        state.histories.put_back(index, direction, next);
        return Err(format!("the next step to {} is {}, not {}", direction.verb(), found.0, tx.0));
    }
    if let Err(e) = state.apply_raw(patch, actor) {
        state.histories.put_back(index, direction, next);
        return Err(e.to_string());
    }
    state.histories.land(index, direction, next);
    Ok(())
}

fn direction_of(stack: Stack) -> Direction {
    match stack {
        Stack::Undo => Direction::Undo,
        Stack::Redo => Direction::Redo,
    }
}
