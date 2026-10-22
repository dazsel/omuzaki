//! A whole journalled session: what the writer makes durable, what the keeper saves, and what opening again finds,
//! after a clean close and after a crash.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc::Project;
use omuzaki_doc::journal::{
    JournalOptions, ProjectJournal, Reader, Record, SessionStart, SystemWall, journal_dir, reopen,
};
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Patch};
use omuzaki_history_api::{HistoryEntry, HistoryExport, JournalSink, JournalStatus, Lsn};
use omuzaki_ops_api::{Actor, OpRecord, TxId};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(10);

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
    let reopened = reopen(dir).unwrap();
    let start = SessionStart::new("test", "0.0.0", base, reopened.history, Arc::new(SystemWall));
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
    let again = reopen(tmp.path()).unwrap();
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

    let reopened = reopen(tmp.path()).unwrap();
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
    let reopened = reopen(tmp.path()).unwrap();
    let start = SessionStart::new("test", "0.0.0", Lsn(0), reopened.history, Arc::new(SystemWall));
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
