//! Bringing the executor back after opening (journal.md 8.3): replay what a crash left, then close every transaction
//! left open, so the session starts at "the state after the last completed transaction".

use omuzaki_doc::journal::End;
use omuzaki_history_api::{HistoryEntry, Lsn};
use omuzaki_ops::Executor;

/// What opening a project had to do to bring it back after Omuzaki stopped without closing it. Empty after a clean
/// close, apart from transactions an agent had left open, which are always closed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecoveryReport {
    /// Whether the last session ended without closing (a crash, a kill, a power cut).
    pub unclean: bool,
    /// Journal records replayed over the snapshot.
    pub replayed: usize,
    /// Why replay stopped early, when a record didn't apply to what came before it. Nothing after it was used.
    pub stopped: Option<String>,
    /// Valid-looking records found after damage in the journal, which were not trusted (0 after an ordinary crash).
    pub untrusted: usize,
    /// Transactions left open, rolled back: their labels.
    pub rolled_back: Vec<String>,
    /// Transactions left open that later work depended on, so they were kept as one undo step each: their labels.
    pub kept: Vec<String>,
}

impl RecoveryReport {
    /// Whether recovery did anything the person should hear about.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// One line for the person: what was brought back and what wasn't kept.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.unclean {
            parts.push(format!("Omuzaki closed unexpectedly; {} saved changes were brought back", self.replayed));
        }
        if !self.rolled_back.is_empty() {
            parts.push(format!("unfinished changes not kept: {}", self.rolled_back.join(", ")));
        }
        if !self.kept.is_empty() {
            parts.push(format!("unfinished changes kept because later work uses them: {}", self.kept.join(", ")));
        }
        if let Some(why) = &self.stopped {
            parts.push(format!("the journal stopped making sense here, and nothing after it was used: {why}"));
        }
        if self.untrusted > 0 {
            parts.push(format!("the journal was damaged; {} later changes couldn't be trusted", self.untrusted));
        }
        parts.join(". ")
    }
}

/// Replays `tail` (the records after the snapshot the executor was restored from) and notes how the journal ended.
pub(crate) fn replay(executor: &Executor, tail: &[(Lsn, HistoryEntry)], end: Option<End>, report: &mut RecoveryReport) {
    report.unclean = end.is_some();
    match executor.replay(tail) {
        Ok(done) => report.replayed = done.replayed,
        Err(stopped) => {
            report.replayed = tail.iter().take_while(|(lsn, _)| *lsn < stopped.lsn).count();
            report.stopped = Some(stopped.to_string());
        }
    }
    if let Some(End::Damaged { later_valid, .. }) = end {
        report.untrusted = later_valid;
    }
}

/// Closes every transaction the executor holds open, newest first: rolled back if its revert still applies, else
/// committed, because committed work by someone else was built on it. Replay applies these as it applies the
/// journal's own rollbacks and commits; they aren't journalled, because the session's base snapshot is taken after.
pub(crate) fn close_open(executor: &Executor, at: Lsn, report: &mut RecoveryReport) {
    let mut open = executor.history_export().open;
    open.sort_by_key(|tx| std::cmp::Reverse(tx.tx));
    for tx in open {
        let rollback = HistoryEntry::Rollback { tx: tx.tx, patch: tx.patch.inverse() };
        if executor.replay(&[(at, rollback)]).is_ok() {
            report.rolled_back.push(tx.label);
        } else {
            // A commit only closes the transaction, so it can't fail for one the executor holds open.
            let committed = executor.replay(&[(at, HistoryEntry::Commit { tx: tx.tx })]);
            debug_assert!(committed.is_ok(), "committing open transaction {} failed: {committed:?}", tx.tx.0);
            report.kept.push(tx.label);
        }
    }
}
