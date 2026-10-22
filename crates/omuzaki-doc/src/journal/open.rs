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

use std::collections::BTreeMap;
use std::path::Path;

use omuzaki_history_api::branch::BranchId;
use omuzaki_history_api::{HistoryEntry, HistoryExport, Lsn, RunningJob, SnapshotId, SnapshotInfo, Trigger, WallClock};

use super::{End, JournalError, Reader, Record, Scan, SystemWall, journal_dir_in};
use crate::Project;
use crate::branch::branch_root;
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
    /// The highest job id the project ever started (0: none): new jobs are numbered after it, so an id is never
    /// reused (ADR 0036 decision 10).
    pub last_job: u64,
    /// Jobs that started and never ended, oldest first: they were running when Omuzaki stopped. They are never
    /// re-run; the host journals an `Interrupted` end for each and deletes its scratch folder.
    pub interrupted: Vec<RunningJob>,
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
    reopen_on(dir, snapshots, mode, &BranchId::main())
}

/// [`reopen_with`] for the folder lane of `branch` (design branches.md 5.2): the folder holds that branch's state,
/// so its journal (under the branch's root) and its snapshots are read, and ADR 0021's rules apply to the folder.
///
/// # Errors
/// As [`reopen_with`].
pub fn reopen_on(
    dir: &Path,
    snapshots: &Snapshotter,
    mode: Recover,
    branch: &BranchId,
) -> Result<Reopened, JournalError> {
    let snapshot_error = |e: omuzaki_history_api::SnapshotError| JournalError::Snapshot(e.to_string());
    let journal = journal_dir_in(&branch_root(dir, branch));
    let Some((info, base, history)) = snapshots.newest_loadable_on(branch).map_err(snapshot_error)? else {
        // Never journalled (or its snapshots are gone): the files are all there is. The journal's numbering continues.
        let scan = scan(&journal, Lsn(0))?;
        let last_lsn = scan.records.last().map_or(Lsn(0), |(lsn, _)| *lsn);
        let project = Project::open(dir)?;
        let (last_job, interrupted) = jobs(None, &scan.records);
        return Ok(Reopened {
            project,
            history: HistoryExport::empty(),
            tail: Vec::new(),
            last_lsn,
            recovery: None,
            last_job,
            interrupted,
        });
    };
    let scan = scan(&journal, info.lsn)?;
    let last_lsn = scan.records.last().map_or(info.lsn, |(lsn, _)| *lsn);
    let closed = matches!(scan.records.last(), Some((_, Record::Close { .. })));
    let (last_job, interrupted) = jobs(Some(&info), &scan.records);
    if closed {
        let project = Project::open(dir)?;
        return Ok(Reopened { project, history, tail: Vec::new(), last_lsn, recovery: None, last_job, interrupted });
    }
    // Unclean. First keep the folder exactly as the crash left it, before anything is written.
    let now = SystemWall.now();
    let facts = Facts::new(last_lsn, "recovery", Trigger::PreRecovery, now)
        .with_branch(branch.clone())
        .pinned_until(omuzaki_history_api::WallTime(now.0.saturating_add(PRE_RECOVERY_PIN_MS)));
    let kept = snapshots.take_raw(facts, &HistoryExport::empty()).map_err(snapshot_error)?.info.id;
    let recovery = Some(Recovery { base: info.id, kept, mode, base_lsn: info.lsn, end: scan.end });
    if mode == Recover::Disk {
        let project = Project::open(dir)?;
        let history = HistoryExport::empty();
        return Ok(Reopened { project, history, tail: Vec::new(), last_lsn, recovery, last_job, interrupted });
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
    Ok(Reopened { project, history, tail, last_lsn, recovery, last_job, interrupted })
}

/// Opens the lane of `branch` beside the folder (design branches.md 5.2): a side lane, whose state lives only in its
/// journal and snapshots. The folder is never read or written. The base is the newest snapshot the branch's lane took;
/// a branch whose lane never ran starts from `head` (its head point's project and history). A journal that ended
/// without `close` is recovered by the headless rules: its tail is replayed, and the caller rolls the transactions
/// still open back and reports it ([`Reopened::recovery`] is set, with the base as what is kept).
///
/// # Errors
/// [`JournalError::Snapshot`] if the snapshots can't be listed; what `head` returns; as [`Reader::scan`].
pub fn reopen_side(
    dir: &Path,
    snapshots: &Snapshotter,
    branch: &BranchId,
    head: impl FnOnce() -> Result<(Project, HistoryExport), JournalError>,
) -> Result<Reopened, JournalError> {
    let snapshot_error = |e: omuzaki_history_api::SnapshotError| JournalError::Snapshot(e.to_string());
    let journal = journal_dir_in(&branch_root(dir, branch));
    let Some((info, project, history)) = snapshots.newest_loadable_on(branch).map_err(snapshot_error)? else {
        let scan = scan(&journal, Lsn(0))?;
        let last_lsn = scan.records.last().map_or(Lsn(0), |(lsn, _)| *lsn);
        let (project, history) = head()?;
        let (last_job, interrupted) = jobs(None, &scan.records);
        return Ok(Reopened { project, history, tail: Vec::new(), last_lsn, recovery: None, last_job, interrupted });
    };
    let scan = scan(&journal, info.lsn)?;
    let last_lsn = scan.records.last().map_or(info.lsn, |(lsn, _)| *lsn);
    let (last_job, interrupted) = jobs(Some(&info), &scan.records);
    if matches!(scan.records.last(), Some((_, Record::Close { .. }))) || scan.records.is_empty() {
        return Ok(Reopened { project, history, tail: Vec::new(), last_lsn, recovery: None, last_job, interrupted });
    }
    let recovery =
        Some(Recovery { base: info.id, kept: info.id, mode: Recover::Restore, base_lsn: info.lsn, end: scan.end });
    let tail = scan
        .records
        .into_iter()
        .filter_map(|(lsn, record)| match record {
            Record::Entry(entry) => Some((lsn, entry)),
            _ => None,
        })
        .collect();
    Ok(Reopened { project, history, tail, last_lsn, recovery, last_job, interrupted })
}

/// The highest job id `base` and `records` hold, and the jobs that never ended: those running at `base` or started
/// in `records`, less those `records` end.
fn jobs(base: Option<&SnapshotInfo>, records: &[(Lsn, Record)]) -> (u64, Vec<RunningJob>) {
    let mut last = base.map_or(0, |info| info.last_job);
    let mut running: BTreeMap<u64, String> =
        base.into_iter().flat_map(|info| &info.running_jobs).map(|job| (job.job, job.op.clone())).collect();
    for (_, record) in records {
        match record {
            Record::Entry(HistoryEntry::JobStarted { job, op, .. }) => {
                last = last.max(job.0);
                running.insert(job.0, op.clone());
            }
            Record::Entry(HistoryEntry::JobEnded { job, .. }) => drop(running.remove(&job.0)),
            _ => {}
        }
    }
    last = last.max(running.keys().next_back().copied().unwrap_or(0));
    (last, running.into_iter().map(|(job, op)| RunningJob::new(job, op)).collect())
}

/// The journal's records after `from`; a project that was never journalled has no folder and no records.
fn scan(journal: &Path, from: Lsn) -> Result<Scan, JournalError> {
    if journal.exists() { Reader::scan(journal, from) } else { Ok(Scan::empty()) }
}
