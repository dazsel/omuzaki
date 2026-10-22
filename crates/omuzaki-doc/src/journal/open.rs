//! Opening a project through its journal (journal.md 8.1 to 8.5): which state is the truth, and what recovery has to
//! replay.
//!
//! - **Clean** (the newest session ended with its `close` record, or there is no journal): the folder is the truth.
//!   Files edited while Omuzaki was closed (a `git pull`, an editor) simply become the next session's base. The history
//!   comes from the newest snapshot, so undo and redo survive the restart.
//! - **Unclean** (the newest session has no `close`): the files may trail the journal by up to two seconds, or hold a
//!   save that a crash cut short, so they are never trusted. The base is the newest snapshot that loads, and the
//!   records after it are handed back for the executor to replay (`Executor::restore` then `Executor::replay`), which
//!   brings back the document and every actor's undo and redo stacks.
//!
//! With [`Recover::Restore`] (the default, and what headless hosts do) an unclean open first keeps the folder as it is
//! in a raw pre-recovery snapshot (pinned 30 days), then writes the folder to match the base exactly
//! ([`Project::write_exact`]), which also clears a save the crash cut short. The keeper's saves then bring the files
//! to the replayed state. With [`Recover::Disk`] the files are opened as they are and nothing is replayed.
//!
//! The new session's base snapshot, which covers whatever recovery did, is written by
//! [`ProjectJournal::start`](super::ProjectJournal::start).

use std::path::Path;

use omuzaki_history_api::{HistoryEntry, HistoryExport, Lsn, SnapshotId, Trigger, WallClock};

use super::{End, JournalError, Reader, Record, Scan, SystemWall, journal_dir};
use crate::Project;
use crate::snapshot::{Facts, Snapshotter};

/// How long a pre-recovery snapshot is pinned: 30 days, in milliseconds.
const PRE_RECOVERY_PIN_MS: u64 = 30 * 24 * 3600 * 1000;

/// What an unclean open does with the files (journal.md 8.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Recover {
    /// Restore to the last committed change: the base snapshot plus the journal (the default).
    #[default]
    Restore,
    /// Open the files as they are (`--recover disk`): nothing is replayed, and the journal's unsaved tail is kept only
    /// in the pre-recovery snapshot's journal.
    Disk,
}

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
    /// The raw snapshot of the folder as the crash left it, pinned 30 days: where the pre-recovery state is kept.
    pub kept: SnapshotId,
    /// What was chosen.
    pub mode: Recover,
    /// The LSN it covers.
    pub base_lsn: Lsn,
    /// How the journal ended: a torn tail is a crash; damage is reported to the person.
    pub end: End,
}

/// Opens the project in `dir` and reads its journal (see the [module docs](self)), restoring after a crash. Taking the
/// folder's lock is the caller's job ([`ProjectLock`](crate::ProjectLock)), before calling this.
///
/// # Errors
/// As [`reopen_with`].
pub fn reopen(dir: &Path, snapshots: &Snapshotter) -> Result<Reopened, JournalError> {
    reopen_with(dir, snapshots, Recover::Restore)
}

/// [`reopen`], choosing what an unclean open does with the files.
///
/// # Errors
/// [`JournalError::Store`] if the folder doesn't open as a project (a clean open, or [`Recover::Disk`]), or can't be
/// written to match the base; [`JournalError::TooNew`] for a journal a newer Omuzaki wrote (the project must not be
/// edited here); [`JournalError::Io`] and [`JournalError::BadPayload`] from reading the journal;
/// [`JournalError::Snapshot`] if the snapshots can't be listed or the pre-recovery snapshot can't be kept.
pub fn reopen_with(dir: &Path, snapshots: &Snapshotter, mode: Recover) -> Result<Reopened, JournalError> {
    let snapshot_error = |e: omuzaki_history_api::SnapshotError| JournalError::Snapshot(e.to_string());
    let journal = journal_dir(dir);
    let Some((info, base, history)) = snapshots.newest_loadable().map_err(snapshot_error)? else {
        // Never journalled (or its snapshots are gone): the files are all there is. The journal's numbering continues.
        let scan = scan(&journal, Lsn(0))?;
        let last_lsn = scan.records.last().map_or(Lsn(0), |(lsn, _)| *lsn);
        let project = Project::open(dir)?;
        return Ok(Reopened { project, history: HistoryExport::empty(), tail: Vec::new(), last_lsn, recovery: None });
    };
    let scan = scan(&journal, info.lsn)?;
    let last_lsn = scan.records.last().map_or(info.lsn, |(lsn, _)| *lsn);
    let closed = matches!(scan.records.last(), Some((_, Record::Close { .. })));
    if closed {
        return Ok(Reopened { project: Project::open(dir)?, history, tail: Vec::new(), last_lsn, recovery: None });
    }
    // Unclean. First keep the folder exactly as the crash left it, before anything is written.
    let now = SystemWall.now();
    let facts = Facts::new(last_lsn, "recovery", Trigger::PreRecovery, now)
        .pinned_until(omuzaki_history_api::WallTime(now.0.saturating_add(PRE_RECOVERY_PIN_MS)));
    let kept = snapshots.take_raw(facts, &HistoryExport::empty()).map_err(snapshot_error)?.info.id;
    let recovery = Some(Recovery { base: info.id, kept, mode, base_lsn: info.lsn, end: scan.end });
    if mode == Recover::Disk {
        let project = Project::open(dir)?;
        return Ok(Reopened { project, history: HistoryExport::empty(), tail: Vec::new(), last_lsn, recovery });
    }
    let mut project = base;
    project.write_exact()?;
    let tail = scan
        .records
        .into_iter()
        .filter_map(|(lsn, record)| match record {
            Record::Entry(entry) => Some((lsn, entry)),
            _ => None,
        })
        .collect();
    Ok(Reopened { project, history, tail, last_lsn, recovery })
}

/// The journal's records after `from`; a project that was never journalled has no folder and no records.
fn scan(journal: &Path, from: Lsn) -> Result<Scan, JournalError> {
    if journal.exists() { Reader::scan(journal, from) } else { Ok(Scan::empty()) }
}
