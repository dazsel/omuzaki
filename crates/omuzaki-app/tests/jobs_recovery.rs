//! Jobs across a crash (ADR 0036 decisions 7 and 10, journal.md 8.3): a job's scratch folder goes when it ends; a job
//! running when Omuzaki stopped is ended as interrupted, never re-run, its scratch folder removed and the person told;
//! job ids carry on past it.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_core::time::Seconds;
use omuzaki_doc::journal::{JournalOptions, ProjectJournal, SessionStart, SystemWall, reopen};
use omuzaki_doc::snapshot::{Snapshotter, snapshot_dir};
use omuzaki_history_api::{HistoryEntry, JournalSink};
use omuzaki_ops_api::{
    Actor, FsMode, JobCtx, JobId, JobState, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, WriteMode,
    register_op,
};
use omuzaki_store::DiskStore;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(10);

/// A job that writes a file into its scratch folder and answers with its id.
#[derive(Serialize, Deserialize, JsonSchema)]
struct ScratchJob {}

impl Op for ScratchJob {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.scratch_job",
            summary: "Write a file into the job's scratch folder.",
            kind: OpKind::Job,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        job.scratch()?.create_dir_all()?;
        let file = job.fs_access(&format!(".omuzaki/jobs/{}/out.txt", job.id().0), FsMode::Write)?;
        drop(file.open_write(WriteMode::CreateNew)?);
        Ok(json!(job.id().0))
    }

    fn finish(&self, _ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        Ok(Plan::answer(result))
    }
}
register_op!(ScratchJob);

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

fn options() -> AppOptions {
    AppOptions::studio(person())
}

/// Runs the scratch job to its end and returns its id.
fn run_job(core: &AppCore) -> u64 {
    let started = core.jobs().start(core.request("test.scratch_job", json!({}), Mode::Apply)).unwrap();
    let ended = core.jobs().wait(started.id, Some(WAIT)).unwrap();
    assert!(matches!(ended.state, JobState::Done { .. }), "{:?}", ended.state);
    started.id.0
}

fn scratch(dir: &Path, job: u64) -> std::path::PathBuf {
    dir.join(".omuzaki").join("jobs").join(job.to_string())
}

/// A session that starts job `job` and dies while it runs: the start is journalled, the end never is, and the job's
/// scratch folder holds half its work.
fn crash_during_job(dir: &Path, job: u64) {
    let snapshots = Arc::new(Snapshotter::new(dir, Arc::new(DiskStore::open(snapshot_dir(dir)).unwrap())));
    let reopened = reopen(dir, &snapshots).unwrap();
    let start =
        SessionStart::new("crashed", "0.0.0", reopened.last_lsn, reopened.history, Arc::new(SystemWall), snapshots)
            .with_last_job(reopened.last_job);
    let journal = ProjectJournal::start(reopened.project, start, JournalOptions::default()).unwrap();
    journal.append(HistoryEntry::JobStarted {
        job: JobId(job),
        op: "test.scratch_job".into(),
        args: json!({}),
        actor: person(),
        on_behalf_of: None,
        at: Seconds(0.0),
    });
    journal.sync(WAIT).unwrap();
    std::fs::create_dir_all(scratch(dir, job)).unwrap();
    std::fs::write(scratch(dir, job).join("out.txt"), "half").unwrap();
    drop(journal);
}

#[test]
fn a_job_running_at_a_crash_is_ended_as_interrupted_with_its_scratch_removed_and_ids_carry_on() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    assert_eq!(run_job(&core), 1);
    assert!(!scratch(tmp.path(), 1).exists(), "a job's scratch folder goes when it ends");
    core.close().unwrap();

    crash_during_job(tmp.path(), 2);

    let core = AppCore::open(tmp.path(), options()).unwrap();
    let recovery = core.recovery();
    assert!(recovery.unclean);
    assert_eq!(recovery.interrupted, ["test.scratch_job (job 2)"]);
    assert!(recovery.summary().contains("jobs stopped part way, not re-run: test.scratch_job (job 2)"), "{recovery:?}");
    assert!(!scratch(tmp.path(), 2).exists(), "the interrupted job's scratch folder is removed");
    assert_eq!(run_job(&core), 3, "ids carry on past the interrupted job");
    core.close().unwrap();

    // The interrupted end is in the journal: a clean open after it reports nothing, and ids still carry on.
    let core = AppCore::open(tmp.path(), options()).unwrap();
    assert!(core.recovery().is_empty(), "{:?}", core.recovery());
    assert_eq!(run_job(&core), 4);
    core.close().unwrap();
}

#[test]
fn a_leftover_scratch_folder_is_removed_at_open() {
    let tmp = tempfile::tempdir().unwrap();
    AppCore::create(tmp.path(), "demo", options()).unwrap().close().unwrap();
    // Left by a job whose start was compacted away, or a removal that failed.
    std::fs::create_dir_all(scratch(tmp.path(), 9).join("deep")).unwrap();
    std::fs::write(scratch(tmp.path(), 9).join("deep/file.bin"), [0u8; 16]).unwrap();
    let core = AppCore::open(tmp.path(), options()).unwrap();
    assert!(!scratch(tmp.path(), 9).exists());
    assert!(core.recovery().is_empty(), "nothing to report: no job of the journal was running");
    core.close().unwrap();
}
