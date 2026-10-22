//! The hand-off to the journal (#251, design `journal.md` §5.1): every change goes to the installed
//! [`JournalSink`] as a [`HistoryEntry`], under the executor's lock, in history order.
//!
//! Without a sink nothing is appended and every edit is admitted, so behaviour is unchanged. The sink is asked to
//! `admit` before an applied edit, an undo or a redo is planned, never before a commit or a rollback: closing a
//! transaction is always possible.

use std::sync::Arc;
use std::time::Duration;

use omuzaki_history_api::{HistoryEntry, JournalSink};
use omuzaki_ops_api::OpError;

/// The installed sink, if any.
#[derive(Clone, Default)]
pub(crate) struct Journal {
    sink: Option<Arc<dyn JournalSink>>,
}

impl Journal {
    pub(crate) fn new(sink: Arc<dyn JournalSink>) -> Self {
        Self { sink: Some(sink) }
    }

    /// Whether a sink is installed: the entries are built only then.
    pub(crate) fn on(&self) -> bool {
        self.sink.is_some()
    }

    /// Hands `entry` to the sink. `make` runs only when one is installed, so an executor without a journal builds no
    /// entry.
    pub(crate) fn append(&self, make: impl FnOnce() -> HistoryEntry) {
        if let Some(sink) = &self.sink {
            sink.append(make());
        }
    }

    /// Whether a new edit may be applied now.
    ///
    /// # Errors
    /// The sink's refusal: saving cannot keep up or has failed.
    pub(crate) fn admit(&self) -> Result<(), OpError> {
        self.sink.as_ref().map_or(Ok(()), |sink| sink.admit())
    }

    /// Waits until everything appended so far is durable. Without a sink there is nothing to wait for.
    ///
    /// # Errors
    /// `Unavailable` naming the journal when it can't write or doesn't catch up within `timeout`.
    pub(crate) fn sync(&self, timeout: Duration) -> Result<(), OpError> {
        let Some(sink) = &self.sink else { return Ok(()) };
        sink.sync(timeout).map(drop).map_err(|error| {
            OpError::unavailable("journal")
                .hint(format!("{error}; nothing was started. Check the disk has space, then try again"))
        })
    }

    /// Whether the sink wants a [`HistoryEntry::Mark`].
    pub(crate) fn wants_mark(&self) -> bool {
        self.sink.as_ref().is_some_and(|sink| sink.wants_mark())
    }
}
