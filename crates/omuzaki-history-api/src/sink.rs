//! The hand-off from the executor to the journal.

use std::time::Duration;

use omuzaki_ops_api::OpError;

use crate::{HistoryEntry, Lsn};

/// Where the executor sends its history. Implemented by the journal in `omuzaki-doc`.
///
/// The executor calls `append` under its state lock, so an implementation takes the entry in O(1) (budget: 2 µs p99)
/// and does its encoding and I/O on its own thread.
pub trait JournalSink: Send + Sync {
    /// Takes `entry`, which comes in history order, and returns the LSN it will have. Never blocks and never does
    /// I/O; it never drops an entry.
    fn append(&self, entry: HistoryEntry) -> Lsn;

    /// Whether a new edit may be applied. The error says why not and what to do. The executor asks before planning an
    /// applied edit, an undo or a redo, and never before a commit or a rollback: closing a transaction is always
    /// possible.
    ///
    /// # Errors
    ///
    /// An [`OpError`] when saving cannot keep up or has failed.
    fn admit(&self) -> Result<(), OpError>;

    /// True when a snapshot is due and the journal wants a [`HistoryEntry::Mark`]. The executor appends one after its
    /// next change, or the sink clears the request when it receives one.
    fn wants_mark(&self) -> bool;

    /// Waits until everything appended before the call is durable, and returns the durable LSN. MCP, the CLI and
    /// scripts acknowledge a change only after this.
    ///
    /// # Errors
    ///
    /// [`SyncError::Timeout`] when `timeout` passes first (the change did apply), [`SyncError::Failing`] when the
    /// journal cannot write.
    fn sync(&self, timeout: Duration) -> Result<Lsn, SyncError>;

    /// The journal's health, for the status banner and the MCP reply.
    fn status(&self) -> JournalStatus;
}

/// The journal's health.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum JournalStatus {
    /// Entries become durable as fast as they arrive.
    Healthy,
    /// The writer is behind ("Saving is behind"); nothing is lost and new edits are still admitted.
    Behind {
        /// Entries queued and not yet durable.
        pending: usize,
    },
    /// Writing fails, or the writer or keeper has died. New edits may be refused.
    Failing {
        /// What went wrong, for the banner.
        reason: String,
    },
}

/// Why [`JournalSink::sync`] did not reach durability.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// The timeout passed. The change applied and is queued, not yet saved.
    #[error("applied, not yet saved: durable up to LSN {}", durable.0)]
    Timeout {
        /// The durable LSN when the wait gave up.
        durable: Lsn,
    },
    /// The journal cannot write.
    #[error("changes can't be saved: {reason}")]
    Failing {
        /// What went wrong.
        reason: String,
    },
}
