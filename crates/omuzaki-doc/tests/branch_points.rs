//! A branch's journal and its points (branches.md 4.4, 5.2, 5.3 and 5.5): a side lane journals under its branch's
//! root and never writes the folder; a point's snapshot is of the document the journal reached or of a given tree,
//! never a base for reopening; a side lane that stopped without closing reopens from its own journal; and a keeper
//! that becomes the folder's rewrites the folder exactly, leaving files outside the tree alone.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc::Project;
use omuzaki_doc::branch::branch_root;
use omuzaki_doc::journal::{
    JournalOptions, ProjectJournal, Reader, Record, SessionStart, SystemWall, journal_dir, journal_dir_in,
    left_open_in, reopen, reopen_side,
};
use omuzaki_doc::snapshot::Snapshotter;
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Patch};
use omuzaki_history_api::branch::BranchId;
use omuzaki_history_api::{HistoryEntry, HistoryExport, JournalSink, Lsn, Trigger};
use omuzaki_ops_api::{Actor, OpRecord, TxId};
use omuzaki_store_mock::MemStore;
use serde_json::json;

const WAIT: Duration = Duration::from_secs(10);

fn side() -> BranchId {
    BranchId::new("9c41d07a5e2b1f30").unwrap()
}

fn note() -> DocPath {
    DocPath::parse("project/note").unwrap()
}

/// A one-op transaction that sets `project/note` from `before` to `after`.
fn set_note(seq: u64, before: Option<&str>, after: &str) -> HistoryEntry {
    let patch = Patch::new().with(note(), before.map(|b| json!(b)), Some(json!(after)));
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

fn note_of(project: &Project) -> Option<String> {
    project.get(&note()).and_then(|v| v.as_str().map(str::to_owned))
}

fn note_on_disk(dir: &Path) -> Option<String> {
    note_of(&Project::open(dir).unwrap())
}

struct Fixture {
    tmp: tempfile::TempDir,
    snapshots: Arc<Snapshotter>,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    std::fs::write(tmp.path().join("notes.md"), "beside the tree\n").unwrap();
    let snapshots = Arc::new(Snapshotter::new(tmp.path(), Arc::new(MemStore::new())));
    Fixture { tmp, snapshots }
}

impl Fixture {
    fn dir(&self) -> &Path {
        self.tmp.path()
    }

    /// Starts the side lane's journal on `project`, as a lane opening on the branch's head would.
    fn side_lane(&self, project: Project, base: Lsn) -> Arc<ProjectJournal> {
        let start =
            SessionStart::new("s", "0.0.0", base, HistoryExport::empty(), Arc::new(SystemWall), self.snapshots.clone())
                .on_branch(side())
                .saving_files(false);
        ProjectJournal::start(
            project,
            start,
            JournalOptions::default().with_save_delays(Duration::ZERO, Duration::ZERO),
        )
        .unwrap()
    }
}

#[test]
fn a_side_lane_journals_under_its_root_and_never_writes_the_folder() {
    let f = fixture();
    let journal = f.side_lane(Project::open(f.dir()).unwrap(), Lsn(0));
    journal.append(set_note(1, None, "side"));
    journal.save_now(WAIT).unwrap();
    assert_eq!(note_on_disk(f.dir()), None, "the folder is the folder lane's");
    assert!(!journal_dir(f.dir()).exists(), "main's journal is untouched");
    let root = branch_root(f.dir(), &side());
    let scan = Reader::scan(&journal_dir_in(&root), Lsn(0)).unwrap();
    assert!(matches!(scan.records.first(), Some((_, Record::Session(_)))));
    assert!(scan.records.iter().any(|(_, r)| matches!(r, Record::Entry(HistoryEntry::Op { .. }))));
    // Its snapshots name the branch.
    let refs = f.snapshots.refs().unwrap();
    assert!(!refs.is_empty() && refs.iter().all(|info| info.branch == side()), "{refs:?}");
    journal.close(HistoryExport::empty(), WAIT).unwrap();
    assert!(!left_open_in(&root));
    assert_eq!(note_on_disk(f.dir()), None);
}

#[test]
fn a_point_is_the_state_the_journal_reached_or_the_tree_it_was_given() {
    let f = fixture();
    let start_tree = Project::open(f.dir()).unwrap().root().clone();
    let journal = f.side_lane(Project::open(f.dir()).unwrap(), Lsn(0));
    journal.append(set_note(1, None, "one"));
    journal.append(set_note(2, Some("one"), "two"));

    let now = journal.take_point(None, "Checkpoint", HistoryExport::empty(), WAIT).unwrap();
    assert_eq!((now.trigger, now.branch.clone(), now.label.as_str()), (Trigger::Point, side(), "Checkpoint"));
    assert_eq!(note_of(&f.snapshots.load(&now.id).unwrap()).as_deref(), Some("two"));

    // A point of an older tree (branching from history): loads back equal to that tree.
    let old = journal.take_point(Some(start_tree.clone()), "From the start", HistoryExport::empty(), WAIT).unwrap();
    assert_eq!(*f.snapshots.load(&old.id).unwrap().root(), start_tree);
    // The keeper's own document is unchanged by it: the next point is "two" again.
    let again = journal.take_point(None, "Again", HistoryExport::empty(), WAIT).unwrap();
    assert_eq!(again.id, now.id, "equal content, one snapshot");

    // Points are never a base: reopening the side lane starts from its session base and replays.
    drop(journal);
    let head =
        || -> Result<(Project, HistoryExport), omuzaki_doc::journal::JournalError> { unreachable!("a base exists") };
    let reopened = reopen_side(f.dir(), &f.snapshots, &side(), head).unwrap();
    assert_ne!(reopened.recovery.as_ref().map(|r| r.base), Some(old.id));
    assert_eq!(reopened.tail.len(), 2, "both edits replay over the session base");
    assert!(reopened.recovery.is_some(), "it stopped without closing");
}

#[test]
fn a_side_lane_that_closed_reopens_as_it_closed_and_one_never_opened_starts_from_its_head() {
    let f = fixture();
    let journal = f.side_lane(Project::open(f.dir()).unwrap(), Lsn(0));
    journal.append(set_note(1, None, "kept"));
    journal.close(HistoryExport::empty(), WAIT).unwrap();
    drop(journal);
    let reopened = reopen_side(f.dir(), &f.snapshots, &side(), || unreachable!("a base exists")).unwrap();
    assert!(reopened.recovery.is_none() && reopened.tail.is_empty());
    assert_eq!(note_of(&reopened.project).as_deref(), Some("kept"));

    let fresh = BranchId::new("0000000000000abc").unwrap();
    let head = Project::open(f.dir()).unwrap();
    let opened = reopen_side(f.dir(), &f.snapshots, &fresh, move || Ok((head, HistoryExport::empty()))).unwrap();
    assert_eq!((opened.last_lsn, opened.recovery.is_none()), (Lsn(0), true));
}

#[test]
fn main_reopens_from_its_own_snapshots_whatever_other_lanes_took() {
    let f = fixture();
    let side_journal = f.side_lane(Project::open(f.dir()).unwrap(), Lsn(0));
    side_journal.append(set_note(1, None, "side"));
    side_journal.close(HistoryExport::empty(), WAIT).unwrap();
    // main has never been journalled: its reopen ignores the side lane's newer snapshots and opens the folder.
    let reopened = reopen(f.dir(), &f.snapshots).unwrap();
    assert_eq!(note_of(&reopened.project), None);
    assert!(reopened.recovery.is_none());
}

#[test]
fn becoming_the_folder_lane_rewrites_the_folder_exactly_and_leaves_other_files_alone() {
    let f = fixture();
    std::fs::create_dir_all(f.dir().join("characters/mia")).unwrap();
    std::fs::write(f.dir().join("characters/mia/face.toml"), "profile = \"round\"\n").unwrap();
    // The side branch's state has no `characters/mia/face` but a note.
    let mut side_state = Project::open(f.dir()).unwrap();
    let mut tree = side_state.root().clone();
    tree.as_object_mut().unwrap().remove("characters");
    tree["project"]["note"] = json!("from the side");
    side_state.reset_to(&tree).unwrap();
    let journal = f.side_lane(side_state, Lsn(0));

    journal.set_folder(true, WAIT).unwrap();
    assert_eq!(note_on_disk(f.dir()).as_deref(), Some("from the side"));
    assert!(!f.dir().join("characters/mia/face.toml").exists(), "a tree file the branch lacks is removed");
    assert_eq!(std::fs::read_to_string(f.dir().join("notes.md")).unwrap(), "beside the tree\n");
    // From then on it saves.
    journal.append(set_note(1, Some("from the side"), "saved"));
    journal.save_now(WAIT).unwrap();
    assert_eq!(note_on_disk(f.dir()).as_deref(), Some("saved"));
    // And once it stops being the folder lane, it stops.
    journal.set_folder(false, WAIT).unwrap();
    journal.append(set_note(2, Some("saved"), "not saved"));
    journal.save_now(WAIT).unwrap();
    assert_eq!(note_on_disk(f.dir()).as_deref(), Some("saved"));
}
