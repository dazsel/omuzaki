//! Content-addressed snapshots (journal.md 7.1 to 7.3): identical content has one id, unchanged files are stored
//! once, assets are referenced and never copied, a ref never names an incomplete snapshot, and a snapshot loads back
//! to the project it was taken of.

// Helpers outside `#[test]` may unwrap.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use common::{p, write};
use omuzaki_doc::Project;
use omuzaki_doc::snapshot::{Facts, Snapshotter};
use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use omuzaki_history_api::{HistoryExport, Lsn, SnapshotError, Trigger, WallTime, register_derived_path};
use omuzaki_store_api::{ContentHash, Provenance, Removal, Store, StoreError, StoreGc};
use omuzaki_store_mock::MemStore;
use serde_json::json;

register_derived_path!("renders/**");

fn facts(lsn: u64) -> Facts {
    Facts::new(Lsn(lsn), "s1", Trigger::Auto, WallTime(1_791_815_525_000 + lsn))
}

/// A project with a character, a few parts and a file beside the tree.
fn project() -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "project.toml", "# my show\nname = \"Show\"\nschema = 1\n");
    write(tmp.path(), "characters/mia/character.toml", "name = \"Mia\"\n");
    write(tmp.path(), "characters/mia/face.toml", "# kept comment\nprofile = \"round\"\n");
    write(tmp.path(), "characters/mia/hair.toml", "kind = \"bob\"\n");
    write(tmp.path(), "notes.md", "# notes beside the tree\n");
    let project = Project::open(tmp.path()).unwrap();
    (tmp, project)
}

fn snapshotter(dir: &std::path::Path, store: &Arc<MemStore>) -> Snapshotter {
    Snapshotter::new(dir, store.clone())
}

#[test]
fn a_snapshot_loads_back_to_the_project_it_was_taken_of() {
    let (tmp, mut project) = project();
    project
        .apply(&Patch::new().with(p("characters/mia/face/profile"), Some(json!("round")), Some(json!("oval"))))
        .unwrap();
    let store = Arc::new(MemStore::new());
    let snaps = snapshotter(tmp.path(), &store);
    let taken = snaps.take_rendered(&project, facts(5), &HistoryExport::empty()).unwrap();

    let loaded = snaps.load(&taken.info.id).unwrap();
    assert_eq!(loaded.root(), project.root());
    assert_eq!(
        loaded.rendered_files().unwrap(),
        project
            .rendered_files()
            .unwrap()
            .into_iter()
            .map(|mut f| {
                // The loaded project read the rendered text as its last-known text, so nothing is pending in it.
                f.changed = false;
                f
            })
            .collect::<Vec<_>>()
    );
    let files = snaps.files(&taken.info.id).unwrap();
    assert_eq!(files["notes.md"], b"# notes beside the tree\n", "files beside the tree are in it");
    assert!(String::from_utf8_lossy(&files["characters/mia/face.toml"]).contains("# kept comment"));
    assert_eq!(snaps.history(&taken.info).unwrap(), HistoryExport::empty());
    assert!(taken.info.verified.as_ref().is_some_and(|v| v.ok));
}

#[test]
fn identical_content_has_one_id_and_a_one_file_edit_writes_one_small_pack() {
    let (a, project_a) = project();
    let (b, project_b) = project();
    let store = Arc::new(MemStore::new());
    let first = snapshotter(a.path(), &store).take_rendered(&project_a, facts(1), &HistoryExport::empty()).unwrap();
    let other = Arc::new(MemStore::new());
    let twin = snapshotter(b.path(), &other).take_rendered(&project_b, facts(9), &HistoryExport::empty()).unwrap();
    assert_eq!(first.info.id, twin.info.id, "two folders with the same files have the same id");
    assert_eq!(first.written.packed, 5);

    let snaps = snapshotter(a.path(), &store);
    let mut project = project_a;
    project.apply(&Patch::new().with(p("characters/mia/hair/kind"), Some(json!("bob")), Some(json!("wolf")))).unwrap();
    let before = store.len();
    let second = snaps.take_rendered(&project, facts(2), &HistoryExport::empty()).unwrap();
    assert_eq!((second.written.packed, second.written.reused), (1, 4), "{:?}", second.written);
    assert_eq!(store.len() - before, 2, "one pack and one manifest (the history export is the same object)");
    let hair = second.manifest.file("characters/mia/hair.toml").unwrap();
    let face = second.manifest.file("characters/mia/face.toml").unwrap();
    assert_ne!(hair.pack, face.pack, "the edited file is in the new pack");
    assert_eq!(
        face.pack,
        first.manifest.file("characters/mia/face.toml").unwrap().pack,
        "the rest point into the parent's"
    );
    let pack = store.get(&hair.pack.unwrap()).unwrap();
    assert_eq!(pack.len() as u64, hair.size, "the new pack holds the edited file only");

    // Taking the same state again changes nothing in the store.
    let again = snaps.take_rendered(&project, facts(3), &HistoryExport::empty()).unwrap();
    assert_eq!(again.info.id, second.info.id);
    assert_eq!(again.written.packed, 0);
}

#[test]
fn a_large_file_beside_the_tree_is_stored_once_across_a_hundred_snapshots() {
    let (tmp, mut project) = project();
    let take: Vec<u8> = (0..5u32 << 20).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    std::fs::create_dir_all(tmp.path().join("takes")).unwrap();
    std::fs::write(tmp.path().join("takes/one.track"), &take).unwrap();
    let store = Arc::new(MemStore::new());
    let snaps = snapshotter(tmp.path(), &store);
    let take_hash = ContentHash::of(&take);
    for lsn in 1..=100u64 {
        let before = project.get(&p("characters/mia/hair/kind")).cloned();
        project
            .apply(&Patch::new().with(p("characters/mia/hair/kind"), before, Some(json!(format!("v{lsn}")))))
            .unwrap();
        let taken = snaps.take_rendered(&project, facts(lsn), &HistoryExport::empty()).unwrap();
        assert_eq!(taken.manifest.file("takes/one.track").unwrap().pack, None, "a large file is its own object");
        assert_eq!(taken.written.objects, usize::from(lsn == 1), "snapshot {lsn}");
    }
    let total: usize = store.hashes().unwrap().iter().map(|h| store.get(h).unwrap().len()).sum();
    assert!(total < take.len() + (1 << 20), "the take is stored once: {total} bytes in the store");
    assert!(store.contains(&take_hash));
}

#[test]
fn assets_are_listed_never_copied_and_derived_paths_and_hidden_names_are_left_out() {
    let (tmp, project) = project();
    let asset = ContentHash::of(b"a texture in the asset store");
    let lock = format!(
        "[[asset]]\nhash = \"{asset}\"\nname = \"skin.png\"\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\nsource = \"authored\"\n"
    );
    write(tmp.path(), "assets.lock", &lock);
    write(tmp.path(), "renders/preview.png", "derived");
    write(tmp.path(), ".git/HEAD", "ref: refs/heads/main");
    write(tmp.path(), ".omuzaki/id", "abc");
    #[cfg(unix)]
    std::os::unix::fs::symlink(tmp.path().join("notes.md"), tmp.path().join("linked.md")).unwrap();
    let store = Arc::new(MemStore::new());
    let taken = snapshotter(tmp.path(), &store).take_rendered(&project, facts(1), &HistoryExport::empty()).unwrap();
    assert_eq!(taken.manifest.assets, [asset]);
    assert!(!store.contains(&asset), "an asset is referenced, never copied");
    let paths: Vec<&str> = taken.manifest.files.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "assets.lock",
            "characters/mia/character.toml",
            "characters/mia/face.toml",
            "characters/mia/hair.toml",
            "notes.md",
            "project.toml"
        ]
    );
}

#[test]
fn an_oversized_file_is_included_with_a_warning() {
    let (tmp, project) = project();
    write(tmp.path(), "takes/big.track", &"x".repeat(4096));
    let store = Arc::new(MemStore::new());
    let snaps = snapshotter(tmp.path(), &store).with_thresholds(1024, 2048);
    let taken = snaps.take_rendered(&project, facts(1), &HistoryExport::empty()).unwrap();
    assert!(taken.manifest.file("takes/big.track").is_some());
    assert!(taken.warnings.iter().any(|w| w.contains("takes/big.track")), "{:?}", taken.warnings);
}

#[test]
fn a_raw_snapshot_keeps_a_folder_that_does_not_open() {
    let (tmp, _) = project();
    write(tmp.path(), "characters/mia/face.toml", "this = = broken\n");
    assert!(Project::open(tmp.path()).is_err());
    let store = Arc::new(MemStore::new());
    let snaps = snapshotter(tmp.path(), &store);
    let taken =
        snaps.take_raw(Facts::new(Lsn(0), "s", Trigger::PreRecovery, WallTime(1)), &HistoryExport::empty()).unwrap();
    assert_eq!(taken.manifest.schema, 1);
    assert_eq!(snaps.files(&taken.info.id).unwrap()["characters/mia/face.toml"], b"this = = broken\n");
    assert!(matches!(snaps.load(&taken.info.id), Err(SnapshotError::Corrupt { .. })), "it is kept, not loadable");
}

/// A store that fails its `n`th put, as a crash between two writes would leave things.
struct FailingStore {
    inner: MemStore,
    puts: AtomicUsize,
    fail_at: usize,
}

impl Store for FailingStore {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        if self.puts.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_at {
            return Err(StoreError::Io("injected".into()));
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

impl StoreGc for FailingStore {
    fn hashes(&self) -> Result<Vec<ContentHash>, StoreError> {
        self.inner.hashes()
    }
    fn stored_at(&self, hash: &ContentHash) -> Result<SystemTime, StoreError> {
        self.inner.stored_at(hash)
    }
    fn remove_if_older(&self, hash: &ContentHash, cutoff: SystemTime) -> Result<Removal, StoreError> {
        self.inner.remove_if_older(hash, cutoff)
    }
    fn purge_retired(&self) -> Result<usize, StoreError> {
        self.inner.purge_retired()
    }
}

#[test]
fn a_ref_never_appears_before_its_objects() {
    let (tmp, mut project) = project();
    write(tmp.path(), "takes/big.track", &"y".repeat(2048));
    project = Project::open(project.dir()).unwrap();
    // Puts are: the large file, the pack, the manifest, the history. A failure at any of them leaves no ref.
    for fail_at in 1..=4 {
        let store = Arc::new(FailingStore { inner: MemStore::new(), puts: AtomicUsize::new(0), fail_at });
        let snaps = Snapshotter::new(tmp.path(), store.clone()).with_thresholds(1024, u64::MAX);
        assert!(snaps.take_rendered(&project, facts(1), &HistoryExport::empty()).is_err(), "put {fail_at}");
        assert_eq!(snaps.refs().unwrap(), [], "put {fail_at}: no ref names an incomplete snapshot");
    }

    // A pack that doesn't read back (the disk flipped a byte) leaves no ref either.
    let store = Arc::new(MemStore::new());
    let snaps = snapshotter(tmp.path(), &store);
    let pack = ContentHash::of(b"never");
    assert!(!store.flip_byte(&pack));
    let good = snaps.take_rendered(&project, facts(1), &HistoryExport::empty()).unwrap();
    let pack = good.manifest.file("project.toml").unwrap().pack.unwrap();
    assert!(store.flip_byte(&pack));
    assert!(matches!(snaps.files(&good.info.id), Err(SnapshotError::Corrupt { .. })), "damage is found on load");
}

#[test]
fn refs_list_newest_first_and_a_damaged_newest_falls_back_to_the_one_before() {
    let (tmp, mut project) = project();
    let store = Arc::new(MemStore::new());
    let snaps = snapshotter(tmp.path(), &store);
    let mut ids = Vec::new();
    for lsn in [3u64, 10, 25] {
        let before = project.get(&p("project/name")).cloned();
        project.apply(&Patch::new().with(p("project/name"), before, Some(json!(format!("Show {lsn}"))))).unwrap();
        ids.push(snaps.take_rendered(&project, facts(lsn), &HistoryExport::empty()).unwrap().info.id);
    }
    let refs = snaps.refs().unwrap();
    assert_eq!(refs.iter().map(|r| r.lsn.0).collect::<Vec<_>>(), [25, 10, 3]);
    assert_eq!(snaps.newest_loadable().unwrap().unwrap().0.id, ids[2]);
    assert!(store.flip_byte(&ids[2].0));
    let (info, loaded, _) = snaps.newest_loadable().unwrap().unwrap();
    assert_eq!((info.lsn, loaded.get(&p("project/name")).cloned()), (Lsn(10), Some(json!("Show 10"))));
}

#[test]
fn a_sweep_keeps_what_refs_reach_and_removes_the_rest_after_the_grace() {
    let now = Arc::new(Mutex::new(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)));
    let clock = now.clone();
    let store = Arc::new(MemStore::with_clock(move || *clock.lock().unwrap()));
    let (tmp, mut project) = project();
    let snaps = snapshotter(tmp.path(), &store);
    let old = snaps.take_rendered(&project, facts(1), &HistoryExport::empty()).unwrap();
    project.apply(&Patch::new().with(p("characters/mia/hair/kind"), Some(json!("bob")), Some(json!("wolf")))).unwrap();
    let new = snaps.take_rendered(&project, facts(2), &HistoryExport::empty()).unwrap();
    let stray = store
        .put(
            b"written for a ref never published",
            omuzaki_store_api::Provenance {
                source: omuzaki_store_api::Source::Authored,
                author: None,
                licence: "LicenseRef-Omuzaki-Internal".into(),
                attribution: None,
                commercial: omuzaki_store_api::Commercial::Ok,
                consent: None,
                private_test: false,
            },
        )
        .unwrap();

    let at = *now.lock().unwrap();
    assert_eq!(snaps.sweep(Duration::from_secs(3600), at).unwrap().removed, [], "within the grace nothing goes");
    snaps.remove_ref(&old.info).unwrap();
    let later = at + Duration::from_secs(7200);
    let swept = snaps.sweep(Duration::from_secs(3600), later).unwrap();
    assert!(swept.removed.contains(&stray));
    assert!(swept.removed.contains(&old.info.id.0), "the old manifest goes with its ref");
    // Everything the remaining snapshot needs is still there, including the parent's pack it points into.
    assert_eq!(snaps.load(&new.info.id).unwrap().root(), project.root());
}
