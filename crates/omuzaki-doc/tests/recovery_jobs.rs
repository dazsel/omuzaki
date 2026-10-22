//! Jobs across a reopen (ADR 0036 decisions 7 and 10, journal.md 8.3): the highest job id carries on, whether the
//! journal tail or a snapshot holds it, and a job that started and never ended is reported as interrupted, whether it
//! started after the newest snapshot or before it.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc::Project;
use omuzaki_doc::journal::{JournalOptions, ProjectJournal, SessionStart, SystemWall, reopen};
use omuzaki_doc::snapshot::Snapshotter;
use omuzaki_history_api::{HistoryEntry, HistoryExport, JournalSink, RunningJob, Trigger};
use omuzaki_ops_api::{Actor, JobId, JobState};
use omuzaki_store_mock::MemStore;
use serde_json::json;

const WAIT: Duration = Duration::from_secs(10);

/// The snapshots of the project in `dir`. Their store outlives every "process" of a test, as the folder's own would.
fn snapshots(dir: &std::path::Path) -> Arc<Snapshotter> {
    static STORES: Mutex<BTreeMap<std::path::PathBuf, Arc<MemStore>>> = Mutex::new(BTreeMap::new());
    let store = STORES.lock().unwrap().entry(dir.to_path_buf()).or_default().clone();
    Arc::new(Snapshotter::new(dir, store))
}

fn start(dir: &std::path::Path) -> Arc<ProjectJournal> {
    let reopened = reopen(dir, &snapshots(dir)).unwrap();
    let start =
        SessionStart::new("test", "0.0.0", reopened.last_lsn, reopened.history, Arc::new(SystemWall), snapshots(dir))
            .with_last_job(reopened.last_job);
    ProjectJournal::start(reopened.project, start, JournalOptions::default()).unwrap()
}

fn started(job: u64, op: &str) -> HistoryEntry {
    HistoryEntry::JobStarted {
        job: JobId(job),
        op: op.into(),
        args: json!({}),
        actor: Actor::Person { name: "pat".into() },
        on_behalf_of: None,
        at: Seconds(1.0),
    }
}

fn ended(job: u64) -> HistoryEntry {
    HistoryEntry::JobEnded { job: JobId(job), state: JobState::Interrupted }
}

#[test]
fn a_job_that_never_ended_is_interrupted_and_ids_carry_on_from_the_tail() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let journal = start(tmp.path());
    journal.append(started(1, "test.quick"));
    journal.append(ended(1));
    journal.append(started(2, "test.slow"));
    journal.sync(WAIT).unwrap();
    drop(journal); // a crash: job 2 never ended

    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    assert!(reopened.recovery.is_some());
    assert_eq!(reopened.last_job, 2);
    assert_eq!(reopened.interrupted, [RunningJob::new(2, "test.slow")]);
}

#[test]
fn a_job_running_at_a_snapshot_is_interrupted_though_its_start_is_before_the_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let journal = start(tmp.path());
    journal.append(started(1, "test.slow"));
    journal.append(started(2, "test.quick"));
    journal.append(ended(2));
    // A named version taken while job 1 runs: the newest snapshot, so the reopen's tail starts after the start.
    let info = journal.take_snapshot(Trigger::Named, Some("mid-job".into()), WAIT).unwrap();
    assert_eq!(info.last_job, 2);
    assert_eq!(info.running_jobs, [RunningJob::new(1, "test.slow")]);
    drop(journal);

    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    assert_eq!(reopened.tail, [], "nothing after the snapshot");
    assert_eq!(reopened.last_job, 2, "carried by the snapshot");
    assert_eq!(reopened.interrupted, [RunningJob::new(1, "test.slow")]);
}

#[test]
fn after_a_clean_close_the_ids_carry_on_and_an_ended_job_is_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let journal = start(tmp.path());
    journal.append(started(7, "test.quick"));
    journal.append(ended(7));
    journal.close(HistoryExport::empty(), WAIT).unwrap();

    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    assert!(reopened.recovery.is_none());
    assert_eq!(reopened.last_job, 7, "carried by the close snapshot");
    assert_eq!(reopened.interrupted, []);

    // The next session's snapshots carry it on too, with no job of their own.
    let next = start(tmp.path());
    next.close(HistoryExport::empty(), WAIT).unwrap();
    assert_eq!(reopen(tmp.path(), &snapshots(tmp.path())).unwrap().last_job, 7);
}

#[test]
fn a_job_reported_once_is_not_reported_again_after_a_second_crash() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let journal = start(tmp.path());
    journal.append(started(3, "test.slow"));
    journal.sync(WAIT).unwrap();
    drop(journal);

    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    assert_eq!(reopened.interrupted.len(), 1);
    // The next session starts (its base snapshot has no job running), journals the interrupted end, and crashes too.
    let next = start(tmp.path());
    next.append(ended(3));
    next.sync(WAIT).unwrap();
    drop(next);

    let again = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    assert_eq!(again.interrupted, []);
    assert_eq!(again.last_job, 3);
}
