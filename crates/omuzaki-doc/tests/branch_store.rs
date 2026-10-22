//! Branches on disk (branches.md 4.1 to 4.7 and 5.5): refs never name a missing point, names stay unique, the bin
//! purges after 30 days, the folder marker survives a crash at each step, a point's tree is the tree it was taken of,
//! and what the refs reach is what a sweep keeps.

// Helpers outside `#[test]` may unwrap.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use common::{p, write};
use omuzaki_doc::Project;
use omuzaki_doc::branch::{
    BIN_DAYS, DiskBranchStore, FolderMarker, FolderState, Refs, branch_root, read_point, write_point,
};
use omuzaki_doc::snapshot::{Facts, Snapshotter};
use omuzaki_doc_api::{DocWrite, Patch};
use omuzaki_history_api::branch::{
    BranchError, BranchId, BranchInfo, BranchName, Contains, Point, PointId, PointKind, Version,
};
use omuzaki_history_api::state::BranchStore;
use omuzaki_history_api::{HistoryExport, Lsn, Trigger, WallTime};
use omuzaki_ops_api::Actor;
use omuzaki_store_api::{ContentHash, Provenance, Store, StoreError, StoreGc};
use omuzaki_store_mock::MemStore;
use serde_json::json;

const T0: u64 = 1_791_815_525_000;
const DAY_MS: u64 = 24 * 3600 * 1000;

fn person() -> Actor {
    Actor::Person { name: "Sam".to_owned() }
}

fn id(n: u8) -> BranchId {
    BranchId::new(format!("{n:016x}")).unwrap()
}

fn name(text: &str) -> BranchName {
    BranchName::new(text).unwrap()
}

/// A store that can be told to refuse writes, as a full disk would.
#[derive(Default)]
struct Flaky {
    inner: MemStore,
    refuse: AtomicBool,
}

impl Store for Flaky {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err(StoreError::Io("injected: the disk is full".into()));
        }
        self.inner.put(bytes, provenance)
    }
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        self.inner.get(hash)
    }
    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.inner.provenance(hash)
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        self.inner.contains(hash)
    }
}

impl StoreGc for Flaky {
    fn hashes(&self) -> Result<Vec<ContentHash>, StoreError> {
        self.inner.hashes()
    }
    fn stored_at(&self, hash: &ContentHash) -> Result<SystemTime, StoreError> {
        self.inner.stored_at(hash)
    }
    fn remove_if_older(
        &self,
        hash: &ContentHash,
        cutoff: SystemTime,
    ) -> Result<omuzaki_store_api::Removal, StoreError> {
        self.inner.remove_if_older(hash, cutoff)
    }
    fn purge_retired(&self) -> Result<usize, StoreError> {
        self.inner.purge_retired()
    }
}

struct World {
    tmp: tempfile::TempDir,
    project: Project,
    store: Arc<Flaky>,
    snapshots: Arc<Snapshotter>,
    refs: Arc<Refs>,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "project.toml", "name = \"Show\"\nschema = 1\n");
    write(tmp.path(), "characters/mia/face.toml", "profile = \"round\"\n");
    let project = Project::open(tmp.path()).unwrap();
    let store = Arc::new(Flaky::default());
    let snapshots = Arc::new(Snapshotter::new(tmp.path(), store.clone()));
    let refs = Arc::new(Refs::new(tmp.path()));
    World { tmp, project, store, snapshots, refs }
}

impl World {
    /// Sets the face profile, as an edit would.
    fn edit(&mut self, profile: &str) {
        let before = self.project.root()["characters"]["mia"]["face"]["profile"].clone();
        let patch = Patch::new().with(p("characters/mia/face/profile"), Some(before), Some(json!(profile)));
        self.project.apply(&patch).unwrap();
    }

    /// A point of the project now on `branch`, number `seq`, after `parents`.
    fn point(&self, branch: &BranchId, seq: u64, parents: Vec<PointId>) -> (Point, PointId) {
        let facts = Facts::new(Lsn(seq), "s", Trigger::Point, WallTime(T0 + seq)).with_branch(branch.clone());
        let snapshot = self.snapshots.take_rendered(&self.project, facts, &HistoryExport::empty()).unwrap().info.id;
        let point =
            Point::new(branch.clone(), seq, snapshot, Lsn(seq), PointKind::Checkpoint, person(), WallTime(T0 + seq))
                .with_parents(parents);
        let point_id = write_point(self.store.as_ref(), &point).unwrap();
        (point, point_id)
    }

    fn branch(&self, branch: &BranchId, branch_name: &str, head: PointId, seq: u64) -> BranchInfo {
        let info = BranchInfo::new(branch.clone(), name(branch_name), head, seq, WallTime(T0 + seq), person());
        self.refs.create(&info, self.store.as_ref()).unwrap();
        info
    }

    fn disk(&self, this: BranchId) -> DiskBranchStore {
        DiskBranchStore::new(this, self.refs.clone(), self.snapshots.clone())
    }
}

#[test]
fn a_ref_never_names_a_point_the_store_lacks() {
    let w = world();
    let (point, point_id) = w.point(&BranchId::main(), 1, Vec::new());
    w.branch(&BranchId::main(), "main", point_id, 1);

    // The point of the next checkpoint can't be stored (the fault falls between the point and the ref).
    w.store.refuse.store(true, Ordering::SeqCst);
    let next = point.clone().with_label("next");
    assert!(matches!(write_point(w.store.as_ref(), &next), Err(BranchError::Io(_))));
    w.store.refuse.store(false, Ordering::SeqCst);
    // A ref naming it is refused, and the old one is unchanged.
    let missing = next.id().unwrap();
    let refused = w.refs.update(&BranchId::main(), w.store.as_ref(), |info| {
        info.head = missing;
        Ok(())
    });
    assert_eq!(refused.unwrap_err(), BranchError::PointNotFound(missing));
    assert_eq!(w.refs.read(&BranchId::main()).unwrap().unwrap().head, point_id);
    // And a new branch can't start from it either.
    let info = BranchInfo::new(id(1), name("aria/oval"), missing, 1, WallTime(T0), person());
    assert_eq!(w.refs.create(&info, w.store.as_ref()), Err(BranchError::PointNotFound(missing)));
    assert!(w.refs.read(&id(1)).unwrap().is_none());
}

#[test]
fn points_read_back_as_written_and_damage_is_caught() {
    let w = world();
    let (point, point_id) = w.point(&BranchId::main(), 1, Vec::new());
    assert_eq!(point_id, point.id().unwrap(), "the id is the hash of the canonical JSON");
    assert_eq!(read_point(w.store.as_ref(), &point_id).unwrap(), point);
    let absent = PointId(ContentHash::of(b"never stored"));
    assert_eq!(read_point(w.store.as_ref(), &absent), Err(BranchError::PointNotFound(absent)));
    assert!(w.store.inner.flip_byte(&point_id.0));
    assert!(matches!(read_point(w.store.as_ref(), &point_id), Err(BranchError::Io(_))));
}

#[test]
fn rename_keeps_the_id_and_names_are_unique_whatever_the_case() {
    let w = world();
    let (_, head) = w.point(&BranchId::main(), 1, Vec::new());
    w.branch(&BranchId::main(), "main", head, 1);
    w.branch(&id(1), "aria/oval", head, 1);
    w.branch(&id(2), "aria/round", head, 1);

    let renamed = w.refs.rename(&id(1), name("aria/oval-a"), Some("Oval A".to_owned()), w.store.as_ref()).unwrap();
    assert_eq!(renamed.id, id(1));
    assert_eq!(w.refs.read(&id(1)).unwrap().unwrap().name, name("aria/oval-a"));
    assert_eq!(w.refs.read(&id(1)).unwrap().unwrap().title.as_deref(), Some("Oval A"));
    let taken = w.refs.rename(&id(1), name("aria/round"), None, w.store.as_ref());
    assert_eq!(taken, Err(BranchError::NameTaken(name("aria/round"))));
    let again = BranchInfo::new(id(3), name("aria/round"), head, 1, WallTime(T0), person());
    assert_eq!(w.refs.create(&again, w.store.as_ref()), Err(BranchError::NameTaken(name("aria/round"))));
    assert!(matches!(
        w.refs.rename(&BranchId::main(), name("trunk"), None, w.store.as_ref()),
        Err(BranchError::Refused(_))
    ));
    assert_eq!(w.refs.by_name(&name("aria/oval-a")).unwrap().map(|b| b.id), Some(id(1)));
    // main first, then by when each was made.
    let order: Vec<BranchId> = w.refs.list().unwrap().into_iter().map(|b| b.id).collect();
    assert_eq!(order, [BranchId::main(), id(1), id(2)]);
}

#[test]
fn the_bin_keeps_a_branch_thirty_days_and_purging_removes_its_folder() {
    let w = world();
    let (_, head) = w.point(&BranchId::main(), 1, Vec::new());
    w.branch(&BranchId::main(), "main", head, 1);
    w.branch(&id(1), "aria/oval", head, 1);
    std::fs::create_dir_all(branch_root(w.tmp.path(), &id(1)).join("journal")).unwrap();

    assert!(matches!(w.refs.delete(&BranchId::main(), WallTime(T0), w.store.as_ref()), Err(BranchError::Refused(_))));
    w.refs.delete(&id(1), WallTime(T0), w.store.as_ref()).unwrap();
    assert_eq!(w.refs.by_name(&name("aria/oval")).unwrap(), None, "a binned branch is not found by name");
    // Its name is free while it is in the bin, and bringing it back is then refused.
    w.branch(&id(2), "aria/oval", head, 1);
    assert_eq!(w.refs.undelete(&id(1), w.store.as_ref()), Err(BranchError::NameTaken(name("aria/oval"))));
    w.refs.rename(&id(2), name("aria/oval-b"), None, w.store.as_ref()).unwrap();
    assert!(w.refs.undelete(&id(1), w.store.as_ref()).unwrap().deleted.is_none());
    w.refs.delete(&id(1), WallTime(T0), w.store.as_ref()).unwrap();

    let early = w.refs.purge(WallTime(T0 + (BIN_DAYS * DAY_MS) - 1)).unwrap();
    assert!(early.is_empty(), "29 days and a bit: still in the bin");
    assert!(branch_root(w.tmp.path(), &id(1)).exists());
    assert_eq!(w.refs.purge(WallTime(T0 + BIN_DAYS * DAY_MS)).unwrap(), [id(1)]);
    assert!(!branch_root(w.tmp.path(), &id(1)).exists(), "the ref, journal and archive go");
    assert!(w.refs.read(&id(1)).unwrap().is_none());
}

#[test]
fn the_folder_marker_is_consistent_after_a_crash_at_each_step() {
    let w = world();
    let dir = w.tmp.path();
    // Never branched: main, ok.
    let none = FolderMarker::read(dir).unwrap();
    assert_eq!(
        (none.branch.clone(), none.state, none.target().clone()),
        (BranchId::main(), FolderState::Ok, BranchId::main())
    );

    // A crash after step 3: the marker says the switch must be finished, towards the target.
    FolderMarker::begin_switch(dir, &BranchId::main(), &id(1)).unwrap();
    let switching = FolderMarker::read(dir).unwrap();
    assert_eq!(switching.state, FolderState::Switching);
    assert_eq!(switching.branch, BranchId::main(), "until it is finished the folder may still be the old branch's");
    assert_eq!(switching.target(), &id(1));
    // Redoing step 3 is the same marker: every step is idempotent.
    FolderMarker::begin_switch(dir, &BranchId::main(), &id(1)).unwrap();
    assert_eq!(FolderMarker::read(dir).unwrap(), switching);

    // A crash after step 6: done.
    FolderMarker::finish_switch(dir, &id(1)).unwrap();
    let done = FolderMarker::read(dir).unwrap();
    assert_eq!((done.branch.clone(), done.state, done.target().clone()), (id(1), FolderState::Ok, id(1)));
    // No temporary file is left behind at any step.
    let names: Vec<String> = std::fs::read_dir(dir.join(".omuzaki/branches"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["folder.toml"]);
}

#[test]
fn a_points_tree_is_the_tree_its_snapshot_was_taken_of() {
    let mut w = world();
    let (_, first) = w.point(&BranchId::main(), 1, Vec::new());
    let tree_then = w.project.root().clone();
    w.edit("oval");
    let (_, second) = w.point(&BranchId::main(), 2, vec![first]);
    w.branch(&BranchId::main(), "main", second, 2);
    let disk = w.disk(BranchId::main());

    assert_eq!(*disk.tree(&first).unwrap(), tree_then);
    assert_eq!(*disk.tree(&second).unwrap(), *w.project.root());
    // Cached: the same tree again, without the store.
    w.store.refuse.store(true, Ordering::SeqCst);
    assert_eq!(*disk.tree(&first).unwrap(), tree_then);
    assert_eq!(disk.this().unwrap().head, second);
    assert_eq!(disk.branch(&name("main")).unwrap().id, BranchId::main());
    assert!(matches!(disk.branch(&name("nope")), Err(BranchError::NotFound(_))));
    let absent = PointId(ContentHash::of(b"absent"));
    assert_eq!(disk.tree(&absent), Err(BranchError::PointNotFound(absent)));
}

#[test]
fn a_store_for_a_project_that_never_branched_says_so() {
    let w = world();
    let disk = w.disk(BranchId::main());
    let error = disk.this().unwrap_err();
    assert!(error.to_string().contains("not branched"), "{error}");
    assert_eq!(disk.list().unwrap(), []);
}

#[test]
fn reachable_follows_parents_of_parents_and_forgets_purged_branches() {
    let mut w = world();
    let (p1, a) = w.point(&BranchId::main(), 1, Vec::new());
    w.edit("oval");
    let (_, b) = w.point(&BranchId::main(), 2, vec![a]);
    w.edit("square");
    let (_, c) = w.point(&BranchId::main(), 3, vec![b]);
    w.branch(&BranchId::main(), "main", c, 3);
    // A side branch forked at the first point, with one point of its own.
    w.edit("heart");
    let (side_point, s1) = w.point(&id(1), 1, vec![a]);
    let mut side = w.branch(&id(1), "aria/heart", s1, 1);
    side.from = Some(a);
    side.inherits = Contains::new().with(BranchId::main(), Version::new(1, a));
    w.refs.write(&side, w.store.as_ref()).unwrap();

    let reach = w.disk(BranchId::main()).reachable().unwrap();
    for point in [a, b, c, s1] {
        assert!(reach.points.contains(&point), "{point}");
    }
    assert!(reach.snapshots.contains(&p1.snapshot) && reach.snapshots.contains(&side_point.snapshot));
    assert_eq!(reach, w.disk(id(1)).reachable().unwrap(), "deterministic, whichever lane asks");

    // A sweep keeps every reachable point and snapshot, though no snapshot ref names the points.
    for info in w.snapshots.refs().unwrap() {
        w.snapshots.remove_ref(&info).unwrap();
    }
    let later = SystemTime::now() + Duration::from_secs(7200);
    w.snapshots.sweep_keeping(&reach.roots(), Duration::ZERO, later).unwrap();
    let disk = w.disk(BranchId::main());
    for point in [a, b, c, s1] {
        assert!(disk.tree(&point).is_ok(), "{point} survives the sweep");
    }

    // Purged from the bin: its own point goes at the next sweep; main's points stay.
    w.refs.delete(&id(1), WallTime(T0), w.store.as_ref()).unwrap();
    w.refs.purge(WallTime(T0 + BIN_DAYS * DAY_MS)).unwrap();
    let reach = w.disk(BranchId::main()).reachable().unwrap();
    assert!(!reach.points.contains(&s1));
    assert!(reach.points.contains(&a));
    w.snapshots.sweep_keeping(&reach.roots(), Duration::ZERO, later).unwrap();
    assert_eq!(read_point(w.store.as_ref(), &s1), Err(BranchError::PointNotFound(s1)));
    assert!(read_point(w.store.as_ref(), &c).is_ok());
}
