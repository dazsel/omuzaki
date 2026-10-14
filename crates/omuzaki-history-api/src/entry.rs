//! The entries the executor appends to the journal.

use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Actor, OpRecord, TxId};
use serde::{Deserialize, Serialize};

use crate::{HistoryExport, WallTime};

/// A log sequence number: the position of one entry in a project's journal. Starts at 1, grows by one per entry,
/// continues across sessions and is never reused. `Lsn(0)` means "nothing yet".
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Lsn(pub u64);

/// Which of an actor's two history stacks a step sits on.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stack {
    /// Steps that can be undone.
    Undo,
    /// Steps that were undone and can be redone.
    Redo,
}

/// One change to history, in the order the executor made it: what the journal writes and what recovery replays.
///
/// Entries are physical: each carries the [`Patch`] it applied, so replay needs no op code. The executor appends an
/// entry only after the change succeeded, so failed applies and refused undos never appear. Documents are
/// null-free (a `omuzaki-doc` invariant), so a patch never holds `Some(Null)`; the journal's JSON would read
/// it back as an absent value.
///
/// Deliberately exhaustive (`Session` was added with the first journal format, before any journal shipped): replay must handle every kind, and a new kind comes with a journal format version and is a
/// workspace-wide change that the compiler should point out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum HistoryEntry {
    /// A session starts. The executor appends it first in every session's stretch of the journal.
    ///
    /// Runner-clock times (`Begin::at`, `OpRecord::at`) are session-relative seconds: monotonic, so ordering survives
    /// a wall-clock jump, but meaningless across sessions. `started` is the wall time at runner-clock zero, so an
    /// entry's wall time is `started + at` ([`WallTime::after`]). Runner-clock times are always finite, and a journal
    /// writer must refuse a non-finite one with an error: JSON writes it as `null`, which does not read back.
    Session {
        /// The session's id.
        session: String,
        /// The wall time at runner-clock zero.
        started: WallTime,
    },
    /// A transaction opened (`Runner::begin`).
    Begin {
        /// The new transaction.
        tx: TxId,
        /// Its owner, the only actor that may commit or roll it back.
        actor: Actor,
        /// Its label, shown in history.
        label: String,
        /// When, in seconds on the runner's session clock (see [`HistoryEntry::Session`]); always finite.
        at: Seconds,
    },
    /// An op was applied.
    Op {
        /// The log record, with its patch. Shared, so the executor's log and the journal hold one copy.
        record: Arc<OpRecord>,
        /// True for a one-op transaction, which has no `Begin` or `Commit`: this record is the whole transaction.
        commits: bool,
    },
    /// A transaction was committed.
    Commit {
        /// Which.
        tx: TxId,
    },
    /// A transaction was rolled back.
    Rollback {
        /// Which.
        tx: TxId,
        /// The revert patch that was applied.
        patch: Patch,
    },
    /// A step was undone.
    Undo {
        /// Whose history.
        actor: Actor,
        /// The step's transaction.
        tx: TxId,
        /// The patch that was applied.
        patch: Patch,
    },
    /// A step was redone.
    Redo {
        /// Whose history.
        actor: Actor,
        /// The step's transaction.
        tx: TxId,
        /// The patch that was applied.
        patch: Patch,
    },
    /// A step was dropped after a conflict (the executor's no-wedge rule); nothing was applied.
    Dropped {
        /// Whose history.
        actor: Actor,
        /// The dropped step's transaction.
        tx: TxId,
        /// The stack it was dropped from.
        stack: Stack,
    },
    /// The executor's whole history right now (cheap: steps are shared), so a snapshot can carry it. Appended when
    /// [`JournalSink::wants_mark`](crate::JournalSink::wants_mark) is true; never written to the journal file.
    Mark {
        /// The history at this point of the journal.
        history: Arc<HistoryExport>,
    },
}
