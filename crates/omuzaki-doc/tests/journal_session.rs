//! A whole journalled session: what the writer makes durable, what the keeper saves, and what opening again finds,
//! after a clean close and after a crash.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc::Project;
use omuzaki_doc::journal::{
    JournalOptions, ProjectJournal, Reader, Record, Recover, SessionStart, SystemWall, archive_dir, journal_dir,
    reopen, reopen_with,
};
use omuzaki_doc::snapshot::Snapshotter;
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Patch};
use omuzaki_history_api::{HistoryEntry, HistoryExport, JournalSink, JournalStatus, Lsn};
use omuzaki_ops_api::{Actor, OpRecord, TxId};
use omuzaki_store_mock::MemStore;
use serde_json::json;

const WAIT: Duration = Duration::from_secs(10);

/// The snapshots of the project in `dir`. Their store outlives every "process" of a test, as the folder's own would.
fn snapshots(dir: &std::path::Path) -> Arc<Snapshotter> {
    static STORES: Mutex<BTreeMap<std::path::PathBuf, Arc<MemStore>>> = Mutex::new(BTreeMap::new());
    let store = STORES.lock().unwrap().entry(dir.to_path_buf()).or_default().clone();
    Arc::new(Snapshotter::new(dir, store))
}

fn path(p: &str) -> DocPath {
    DocPath::parse(p).unwrap()
}

/// A one-op transaction that sets `project/note` from `before` to `after`.
fn set_note(seq: u64, before: Option<&str>, after: &str) -> HistoryEntry {
    let patch = Patch::new().with(path("project/note"), before.map(|b| json!(b)), Some(json!(after)));
    let actor = Actor::Person { name: "pat".into() };
    let record = OpRecord::new(
        seq,
        "project.set_note",
        json!({"note": after}),
        actor,
        TxId(seq),
        Seconds(0.5),
        patch,
        DocVersion(seq),
    );
    HistoryEntry::Op { record: Arc::new(record), commits: true }
}

fn start(dir: &std::path::Path, base: Lsn) -> Arc<ProjectJournal> {
    let reopened = reopen(dir, &snapshots(dir)).unwrap();
    let start = SessionStart::new("test", "0.0.0", base, reopened.history, Arc::new(SystemWall), snapshots(dir));
    // Saves almost at once, so the test can see the files follow.
    let options = JournalOptions::default().with_save_delays(Duration::from_millis(5), Duration::from_millis(20));
    ProjectJournal::start(reopened.project, start, options).unwrap()
}

fn note_on_disk(dir: &std::path::Path) -> Option<String> {
    let project = Project::open(dir).unwrap();
    project.get(&path("project/note")).and_then(|v| v.as_str().map(str::to_owned))
}

#[test]
fn a_closed_session_reopens_from_its_files_with_nothing_to_replay() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let journal = start(tmp.path(), Lsn(0));
    assert_eq!(journal.append(set_note(1, None, "one")), Lsn(2), "the session record is LSN 1");
    assert_eq!(journal.append(set_note(2, Some("one"), "two")), Lsn(3));
    assert_eq!(journal.sync(WAIT).unwrap(), Lsn(3));
    journal.save_now(WAIT).unwrap();
    assert_eq!(note_on_disk(tmp.path()).as_deref(), Some("two"), "the keeper saved the files");
    assert_eq!(journal.status(), JournalStatus::Healthy);

    journal.close(HistoryExport::empty(), WAIT).unwrap();
    assert!(journal.admit().is_err(), "a closed journal admits nothing");
    let again = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    assert!(again.recovery.is_none(), "clean");
    assert_eq!(again.tail, []);
    assert_eq!(again.last_lsn, Lsn(4), "session, two ops, close");
    assert_eq!(again.project.get(&path("project/note")), Some(&json!("two")));
}

#[test]
fn after_a_crash_the_snapshot_and_the_journal_rebuild_what_the_files_missed() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let journal = start(tmp.path(), Lsn(0));
    journal.append(set_note(1, None, "one"));
    journal.sync(WAIT).unwrap();
    // The process dies: the threads stop, nothing closes. Whatever the keeper saved or not, the files aren't trusted.
    drop(journal);

    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    let recovery = reopened.recovery.expect("the session didn't close");
    assert_eq!(recovery.base_lsn, Lsn(0), "the session's base snapshot");
    assert_eq!(reopened.tail.len(), 1);
    assert_eq!(reopened.tail[0], (Lsn(2), set_note(1, None, "one")));
    assert_eq!(reopened.last_lsn, Lsn(2));
    assert_eq!(reopened.project.get(&path("project/note")), None, "the base is the snapshot; replay adds the rest");

    // The next session continues the numbering, in a segment the reader reaches from the first.
    let next = start(tmp.path(), reopened.last_lsn);
    assert_eq!(next.append(set_note(2, None, "again")), Lsn(4), "base 2, session 3");
    next.sync(WAIT).unwrap();
    drop(next);
    let scan = Reader::scan(&journal_dir(tmp.path()), Lsn(0)).unwrap();
    let lsns: Vec<u64> = scan.records.iter().map(|(lsn, _)| lsn.0).collect();
    assert_eq!(lsns, [1, 2, 3, 4]);
    assert!(matches!(scan.records[2].1, Record::Session(_)));
}

#[test]
fn segments_rotate_without_breaking_the_log() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    let start =
        SessionStart::new("test", "0.0.0", Lsn(0), reopened.history, Arc::new(SystemWall), snapshots(tmp.path()));
    let journal =
        ProjectJournal::start(reopened.project, start, JournalOptions::default().with_segment_bytes(600)).unwrap();
    let mut before = None;
    for seq in 1..=20 {
        let after = format!("note {seq}");
        journal.append(set_note(seq, before.as_deref(), &after));
        journal.sync(WAIT).unwrap();
        before = Some(after);
    }
    journal.close(HistoryExport::empty(), WAIT).unwrap();
    let segments = std::fs::read_dir(journal_dir(tmp.path())).unwrap().count();
    assert!(segments > 2, "{segments} segments");
    let scan = Reader::scan(&journal_dir(tmp.path()), Lsn(0)).unwrap();
    assert_eq!(scan.records.len(), 22, "session, 20 ops, close");
    assert_eq!(note_on_disk(tmp.path()).as_deref(), Some("note 20"));
}

fn mark() -> HistoryEntry {
    HistoryEntry::Mark { history: Arc::new(HistoryExport::empty()) }
}

#[test]
fn segments_behind_the_snapshot_before_the_newest_move_to_the_archive_and_recovery_still_works() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    let start =
        SessionStart::new("test", "0.0.0", Lsn(0), reopened.history, Arc::new(SystemWall), snapshots(tmp.path()));
    let journal =
        ProjectJournal::start(reopened.project, start, JournalOptions::default().with_segment_bytes(600)).unwrap();
    let mut before = None;
    let mut write = |from: u64, to: u64, journal: &ProjectJournal| {
        for seq in from..=to {
            let after = format!("note {seq}");
            journal.append(set_note(seq, before.as_deref(), &after));
            before = Some(after);
        }
        journal.sync(WAIT).unwrap();
    };
    write(1, 15, &journal);
    let first_mark = journal.append(mark());
    write(16, 30, &journal);
    journal.append(mark());
    write(31, 40, &journal);
    journal.save_now(WAIT).unwrap();

    let archived: Vec<_> = std::fs::read_dir(archive_dir(tmp.path())).unwrap().map(|e| e.unwrap().path()).collect();
    assert!(!archived.is_empty(), "the segments behind the first mark were archived");
    let archive = Reader::scan(&archive_dir(tmp.path()), Lsn(0)).unwrap();
    let last_archived = archive.records.last().unwrap().0;
    assert!(last_archived <= first_mark, "{last_archived:?} is behind the snapshot before the newest");
    // The live journal now starts where the archive ends: read from there, as recovery reads from a snapshot.
    let live = Reader::scan(&journal_dir(tmp.path()), last_archived).unwrap();
    assert_eq!(live.records.first().unwrap().0.0, last_archived.0 + 1, "the op log is continuous across the move");

    // A crash now still recovers everything from the newest snapshot and the live journal.
    drop(journal);
    let again = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    assert!(again.recovery.is_some());
    let mut doc = again.project.clone();
    for (_, entry) in &again.tail {
        if let HistoryEntry::Op { record, .. } = entry {
            omuzaki_doc_api::DocWrite::apply(&mut doc, &record.patch).unwrap();
        }
    }
    assert_eq!(doc.get(&path("project/note")), Some(&json!("note 40")));
}

#[test]
fn recovery_keeps_the_folder_as_the_crash_left_it_and_rewrites_it_to_the_base() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let journal = start(tmp.path(), Lsn(0));
    journal.append(set_note(1, None, "one"));
    journal.sync(WAIT).unwrap();
    drop(journal);
    // The crash also cut a layout-changing save short: a file and a folder of one name, and a broken file.
    std::fs::write(tmp.path().join("characters.toml"), "mia = 1\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("characters/mia")).unwrap();
    std::fs::write(tmp.path().join("characters/mia/face.toml"), "profile = \"round\"\n").unwrap();
    std::fs::write(tmp.path().join("notes.md"), "not part of the tree\n").unwrap();
    assert!(Project::open(tmp.path()).is_err(), "the folder as the crash left it doesn't open");

    let snaps = snapshots(tmp.path());
    let disk = reopen_with(tmp.path(), &snaps, Recover::Disk);
    assert!(disk.is_err(), "the files as they are can't be opened, so only a restore is offered");
    let reopened = reopen(tmp.path(), &snaps).unwrap();
    let recovery = reopened.recovery.unwrap();
    let kept = snaps.files(&recovery.kept).unwrap();
    assert_eq!(kept["characters.toml"], b"mia = 1\n", "the pre-recovery snapshot keeps the folder exactly");
    let pinned = snaps.refs().unwrap().into_iter().find(|r| r.id == recovery.kept).unwrap();
    assert!(pinned.keep_until.is_some(), "and is pinned");
    assert!(Project::open(tmp.path()).is_ok(), "the folder was written to match the base");
    assert!(!tmp.path().join("characters.toml").exists() && !tmp.path().join("characters").exists());
    assert_eq!(std::fs::read_to_string(tmp.path().join("notes.md")).unwrap(), "not part of the tree\n");
    assert_eq!(reopened.tail.len(), 1, "and the journal replays over it");
}

#[test]
fn recover_disk_opens_the_files_as_they_are() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let journal = start(tmp.path(), Lsn(0));
    journal.append(set_note(1, None, "one"));
    journal.sync(WAIT).unwrap();
    drop(journal);
    std::fs::write(tmp.path().join("project.toml"), "name = \"edited by hand\"\nschema = 1\n").unwrap();
    let reopened = reopen_with(tmp.path(), &snapshots(tmp.path()), Recover::Disk).unwrap();
    assert_eq!(reopened.recovery.unwrap().mode, Recover::Disk);
    assert!(reopened.tail.is_empty());
    assert_eq!(reopened.project.get(&path("project/name")), Some(&json!("edited by hand")));
}
