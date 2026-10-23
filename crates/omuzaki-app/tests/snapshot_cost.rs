//! What a snapshot costs on a real disk (journal.md 13; #364): a 500-file project with 10 files changed since the
//! parent, into the `DiskStore` the app uses. The design's budget is 50 ms on ext4 in a release build; the ceiling
//! here is wide enough for a debug build on a shared CI runner, and the measured number goes in the pull request.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_doc::Project;
use omuzaki_doc::snapshot::{Facts, Snapshotter, snapshot_dir};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_history_api::{HistoryExport, Lsn, Trigger, WallTime};
use omuzaki_store::DiskStore;
use serde_json::json;

const FILES: usize = 500;
const CHANGED: usize = 10;

#[test]
fn a_snapshot_of_a_500_file_project_with_10_changes_is_quick() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("project.toml"), "name = \"bench\"\nschema = 1\n").unwrap();
    for i in 0..FILES - 1 {
        let folder = tmp.path().join(format!("characters/c{}", i % 25));
        std::fs::create_dir_all(&folder).unwrap();
        let text = format!("kind = \"lock\"\nindex = {i}\ncurve = [0.1, 0.2, 0.3, 0.4]\ncolour = \"#a0b0c0\"\n");
        std::fs::write(folder.join(format!("part{i}.toml")), text).unwrap();
    }
    let mut project = Project::open(tmp.path()).unwrap();
    assert_eq!(project.rendered_files().unwrap().len(), FILES);

    let store = DiskStore::open(snapshot_dir(tmp.path())).unwrap();
    let snapshots = Snapshotter::new(tmp.path(), Arc::new(store));
    snapshots.take_rendered(&project, facts(0), &HistoryExport::empty()).unwrap();

    let mut times = Vec::new();
    for round in 1..=5u64 {
        for i in 0..CHANGED {
            let path = DocPath::parse(&format!("characters/c{}/part{i}/index", i % 25)).unwrap();
            let before = project.get(&path).cloned();
            project.apply(&Patch::new().with(path, before, Some(json!(round * 1000 + i as u64)))).unwrap();
        }
        let started = Instant::now();
        let taken = snapshots.take_rendered(&project, facts(round), &HistoryExport::empty()).unwrap();
        times.push(started.elapsed());
        assert_eq!(taken.written.packed, CHANGED, "only the changed files are written");
    }
    times.sort();
    let median = times[times.len() / 2];
    println!("snapshot of {FILES} files with {CHANGED} changed: median {median:?} of {times:?}");
    assert!(median < Duration::from_millis(500), "median {median:?}");
}

fn facts(lsn: u64) -> Facts {
    Facts::new(Lsn(lsn), "bench", Trigger::Auto, WallTime(1_791_815_525_000 + lsn))
}
