//! The keeper on a fake clock (#408, journal.md 6): saves debounce at 250 ms after the last change and the files never
//! trail by more than 2 s while changes continue; a mark gives one snapshot holding the mark's history; a close leaves
//! the files equal to the close snapshot with `close` as the last record; compaction never moves a segment the newest
//! snapshot's replay needs; a shadow that disagrees with the executor is reported and stops new edits.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc::Project;
use omuzaki_doc::journal::{
    JournalOptions, ProjectJournal, Reader, Record, SessionStart, SystemWall, archive_dir, journal_dir, reopen,
};
use omuzaki_doc::snapshot::Snapshotter;
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Patch};
use omuzaki_history_api::{HistoryEntry, HistoryExport, JournalSink, JournalStatus, Lsn, Trigger};
use omuzaki_ops_api::{Actor, OpRecord, TxId};
use omuzaki_store_mock::MemStore;
use serde_json::json;

const WAIT: Duration = Duration::from_secs(10);

/// The snapshot store of the project in `dir`: it outlives every "process" of a test, as the folder's own would.
fn store(dir: &Path) -> Arc<MemStore> {
    static STORES: Mutex<BTreeMap<PathBuf, Arc<MemStore>>> = Mutex::new(BTreeMap::new());
    STORES.lock().unwrap().entry(dir.to_path_buf()).or_default().clone()
}

fn snapshots(dir: &Path) -> Arc<Snapshotter> {
    Arc::new(Snapshotter::new(dir, store(dir)))
}

fn start(dir: &Path, clock: &Arc<FakeClock>, options: JournalOptions) -> Arc<ProjectJournal> {
    let reopened = reopen(dir, &snapshots(dir)).unwrap();
    let start =
        SessionStart::new("test", "0.0.0", reopened.last_lsn, reopened.history, Arc::new(SystemWall), snapshots(dir))
            .with_clock(clock.clone());
    ProjectJournal::start(reopened.project, start, options).unwrap()
}

fn project() -> (tempfile::TempDir, Arc<FakeClock>) {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    (tmp, Arc::new(FakeClock::new(Seconds(0.0))))
}

/// A one-op transaction that sets `project/note` from `before` to `after`.
fn set_note(seq: u64, before: Option<&str>, after: &str) -> HistoryEntry {
    let note = DocPath::parse("project/note").unwrap();
    let patch = Patch::new().with(note, before.map(|b| json!(b)), Some(json!(after)));
    let record = OpRecord::new(
        seq,
        "project.set_note",
        json!({"note": after}),
        Actor::Person { name: "pat".into() },
        TxId(seq),
        Seconds(0.0),
        patch,
        DocVersion(seq),
    );
    HistoryEntry::Op { record: Arc::new(record), commits: true }
}

fn note_on_disk(dir: &Path) -> Option<String> {
    let project = Project::open(dir).unwrap();
    project.get(&DocPath::parse("project/note").unwrap()).and_then(|v| v.as_str().map(str::to_owned))
}

fn is_toml(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|ext| ext == "toml")
}

/// The tree's files under `dir`, by `/`-separated path: `.toml` files in folders that aren't hidden.
fn tree_files(dir: &Path, prefix: &str) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(dir.join(prefix)).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if name.starts_with('.') {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            files.extend(tree_files(dir, &rel));
        } else if is_toml(&name) {
            files.insert(rel, std::fs::read(entry.path()).unwrap());
        }
    }
    files
}

/// Moves the clock to `at` seconds and lets the keeper look at it.
fn at(journal: &ProjectJournal, clock: &FakeClock, at: f64) {
    clock.set(Seconds(at));
    journal.tick(WAIT).unwrap();
}

#[test]
fn a_save_follows_250_ms_after_the_last_change() {
    let (tmp, clock) = project();
    let journal = start(tmp.path(), &clock, JournalOptions::default());
    journal.append(set_note(1, None, "one"));
    at(&journal, &clock, 0.0);
    at(&journal, &clock, 0.2);
    assert_eq!(note_on_disk(tmp.path()), None, "not yet: 200 ms after the change");
    at(&journal, &clock, 0.26);
    assert_eq!(note_on_disk(tmp.path()).as_deref(), Some("one"));
}

#[test]
fn while_changes_continue_the_files_trail_by_at_most_two_seconds() {
    let (tmp, clock) = project();
    let journal = start(tmp.path(), &clock, JournalOptions::default());
    let mut before: Option<String> = None;
    let mut first_save = None;
    // A change every 200 ms: never 250 ms of quiet.
    for step in 0..=15u32 {
        let now = f64::from(step) * 0.2;
        clock.set(Seconds(now));
        let note = format!("note {step}");
        journal.append(set_note(u64::from(step) + 1, before.as_deref(), &note));
        journal.tick(WAIT).unwrap();
        before = Some(note);
        if first_save.is_none() && note_on_disk(tmp.path()).is_some() {
            first_save = Some(now);
        }
    }
    let first_save = first_save.expect("the files were saved while changes went on");
    assert!((1.99..=2.01).contains(&first_save), "saved at {first_save} s");
}

#[test]
fn a_mark_gives_one_snapshot_holding_the_mark_s_history() {
    let (tmp, clock) = project();
    let journal = start(tmp.path(), &clock, JournalOptions::default());
    let snaps = snapshots(tmp.path());
    let before = snaps.refs().unwrap().len();
    journal.append(set_note(1, None, "one"));
    let mut history = HistoryExport::empty();
    history.next_tx = TxId(2);
    history.next_seq = 2;
    journal.append(HistoryEntry::Mark { history: Arc::new(history.clone()) });
    journal.tick(WAIT).unwrap();
    let refs = snaps.refs().unwrap();
    assert_eq!(refs.len(), before + 1, "one snapshot");
    let newest = &refs[0];
    assert_eq!((newest.trigger, newest.lsn), (Trigger::Auto, Lsn(2)), "at the op before the mark");
    assert_eq!(snaps.history(newest).unwrap(), history);
    assert_eq!(snaps.load(&newest.id).unwrap().get(&DocPath::parse("project/note").unwrap()), Some(&json!("one")));
}

#[test]
fn a_close_leaves_the_files_equal_to_the_close_snapshot_and_close_last() {
    let (tmp, clock) = project();
    let journal = start(tmp.path(), &clock, JournalOptions::default());
    journal.append(set_note(1, None, "one"));
    journal.append(set_note(2, Some("one"), "two"));
    let id = journal.close(HistoryExport::empty(), WAIT).unwrap();
    let snaps = snapshots(tmp.path());
    let on_disk = tree_files(tmp.path(), "");
    let in_snapshot: BTreeMap<String, Vec<u8>> =
        snaps.files(&id).unwrap().into_iter().filter(|(path, _)| is_toml(path)).collect();
    assert_eq!(on_disk, in_snapshot);
    let scan = Reader::scan(&journal_dir(tmp.path()), Lsn(0)).unwrap();
    assert!(matches!(scan.records.last(), Some((_, Record::Close { snapshot })) if *snapshot == id));
}

#[test]
fn compaction_keeps_what_the_replay_from_either_of_the_two_newest_snapshots_needs() {
    let (tmp, clock) = project();
    // Small segments, so most batches rotate, and a snapshot every few records.
    let options = JournalOptions::default().with_segment_bytes(300).with_snapshot_cadence(Duration::from_secs(600), 3);
    let journal = start(tmp.path(), &clock, options);
    let mut before: Option<String> = None;
    for n in 1..=30u64 {
        let note = format!("note {n}");
        journal.append(set_note(n, before.as_deref(), &note));
        if journal.wants_mark() {
            journal.append(HistoryEntry::Mark { history: Arc::new(HistoryExport::empty()) });
        }
        journal.tick(WAIT).unwrap();
        before = Some(note);
    }
    // Then no snapshot for a while: the replay needs several segments, sealed ones among them.
    for n in 31..=36u64 {
        let note = format!("note {n}");
        journal.append(set_note(n, before.as_deref(), &note));
        journal.tick(WAIT).unwrap();
        before = Some(note);
    }
    drop(journal); // a crash
    assert!(archive_dir(tmp.path()).exists(), "something was compacted");
    // The newest snapshot is damaged: recovery falls back to the one before, whose replay compaction kept too.
    let refs = snapshots(tmp.path()).refs().unwrap();
    let newest = refs.iter().find(|info| info.trigger == Trigger::Auto).unwrap();
    let previous = refs.iter().filter(|info| info.trigger == Trigger::Auto).nth(1).unwrap().lsn;
    assert!(store(tmp.path()).flip_byte(&newest.id.0));

    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    assert_eq!(reopened.recovery.as_ref().unwrap().base_lsn, previous, "the snapshot before the newest");
    let base = reopened.recovery.expect("it didn't close").base_lsn;
    let first = reopened.tail.first().map(|(lsn, _)| *lsn);
    assert!(first.is_none_or(|lsn| lsn.0 == base.0 + 1), "the tail starts right after the base: {first:?} {base:?}");
    // The replay brings the base to the last note.
    let mut project = reopened.project;
    for (_, entry) in &reopened.tail {
        if let HistoryEntry::Op { record, .. } = entry {
            omuzaki_doc_api::DocWrite::apply(&mut project, &record.patch).unwrap();
        }
    }
    assert_eq!(project.get(&DocPath::parse("project/note").unwrap()), Some(&json!("note 36")));
}

#[test]
fn a_shadow_that_disagrees_is_reported_and_stops_new_edits() {
    let (tmp, clock) = project();
    let journal = start(tmp.path(), &clock, JournalOptions::default());
    // The executor says the note was "elsewhere" before; the files never had one.
    journal.append(set_note(1, Some("elsewhere"), "one"));
    let _ = journal.tick(WAIT);
    let JournalStatus::Failing { reason } = journal.status() else { panic!("{:?}", journal.status()) };
    assert!(reason.contains("disagrees"), "{reason}");
    assert!(journal.admit().is_err());
}
