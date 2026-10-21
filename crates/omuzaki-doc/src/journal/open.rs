//! Opening a project through its journal (journal.md 8.1 to 8.3): which state is the truth, and what recovery has to
//! replay.
//!
//! - **Clean** (the newest session ended with its `close` record, or there is no journal): the folder is the truth.
//!   Files edited while Omuzaki was closed (a `git pull`, an editor) simply become the next session's base. The history
//!   comes from the newest snapshot, so undo and redo survive the restart.
//! - **Unclean** (the newest session has no `close`): the files may trail the journal by up to two seconds, or hold a
//!   save that a crash cut short, so they are never trusted. The base is the newest snapshot's tree, and the records
//!   after it are handed back for the executor to replay (`Executor::restore` then `Executor::replay`), which brings
//!   back the document and every actor's undo and redo stacks.
//!
//! Nothing here writes: the new session's base snapshot, which covers whatever recovery did, is written by
//! [`ProjectJournal::start`](super::ProjectJournal::start), and the keeper's first save brings the files in line.

use std::path::Path;

use omuzaki_history_api::{HistoryEntry, HistoryExport, Lsn, SnapshotId};

use super::snapshot::{self, Snapshot};
use super::{End, JournalError, Reader, Record, Scan, journal_dir};
use crate::{Project, StoreError};

/// A project as opening found it: the base to start the executor from and what to replay over it.
#[derive(Debug)]
#[non_exhaustive]
pub struct Reopened {
    /// The document to start from: the files as they are (clean), or the files brought to the newest snapshot's tree
    /// (unclean). The executor gets one copy and the journal's keeper another.
    pub project: Project,
    /// The history to restore the executor with: the newest snapshot's, or empty for a project never opened before.
    pub history: HistoryExport,
    /// The records to replay after restoring, in LSN order: empty unless the last session didn't close.
    pub tail: Vec<(Lsn, HistoryEntry)>,
    /// The last LSN in the journal. The next session's base snapshot covers it.
    pub last_lsn: Lsn,
    /// What recovery found, when the last session didn't close.
    pub recovery: Option<Recovery>,
}

/// What an unclean open found.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Recovery {
    /// The snapshot recovery starts from.
    pub base: SnapshotId,
    /// The LSN it covers.
    pub base_lsn: Lsn,
    /// How the journal ended: a torn tail is a crash; damage is reported to the person.
    pub end: End,
}

/// Opens the project in `dir` and reads its journal (see the [module docs](self)). Taking the folder's lock is the
/// caller's job ([`ProjectLock`](crate::ProjectLock)), before calling this.
///
/// # Errors
/// [`JournalError::Store`] if the folder doesn't open as a project, or a snapshot's tree can't be stored;
/// [`JournalError::TooNew`] for a journal a newer Omuzaki wrote (the project must not be edited here);
/// [`JournalError::Io`] and [`JournalError::BadPayload`] from reading the journal.
pub fn reopen(dir: &Path) -> Result<Reopened, JournalError> {
    let mut project = Project::open(dir)?;
    let journal = journal_dir(dir);
    let Some((base, snapshot)) = snapshot::newest(&snapshot::snapshot_dir(dir))? else {
        // Never journalled (or its snapshots are gone): the files are all there is. The journal's numbering continues.
        let scan = scan(&journal, Lsn(0))?;
        let last_lsn = scan.records.last().map_or(Lsn(0), |(lsn, _)| *lsn);
        return Ok(Reopened { project, history: HistoryExport::empty(), tail: Vec::new(), last_lsn, recovery: None });
    };
    let scan = scan(&journal, snapshot.lsn)?;
    let last_lsn = scan.records.last().map_or(snapshot.lsn, |(lsn, _)| *lsn);
    let closed = matches!(scan.records.last(), Some((_, Record::Close { .. })));
    let Snapshot { root, history, lsn: base_lsn, .. } = snapshot;
    if closed {
        return Ok(Reopened { project, history, tail: Vec::new(), last_lsn, recovery: None });
    }
    project.reset_to(&root).map_err(|e| StoreError::Internal(format!("snapshot {base} can't be stored: {e}")))?;
    let tail = scan
        .records
        .into_iter()
        .filter_map(|(lsn, record)| match record {
            Record::Entry(entry) => Some((lsn, entry)),
            _ => None,
        })
        .collect();
    let recovery = Some(Recovery { base, base_lsn, end: scan.end });
    Ok(Reopened { project, history, tail, last_lsn, recovery })
}

/// The journal's records after `from`; a project that was never journalled has no folder and no records.
fn scan(journal: &Path, from: Lsn) -> Result<Scan, JournalError> {
    if journal.exists() { Reader::scan(journal, from) } else { Ok(Scan::empty()) }
}
