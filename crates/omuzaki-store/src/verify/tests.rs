use std::fs;
use std::time::Duration;

use omuzaki_store_api::{Commercial, Provenance, Store, StoreError};
use omuzaki_store_mock::conformance::{full_provenance, payload, sample_provenance};

use super::*;
use crate::tests::{files_under, flip_first_byte, open};
use crate::{DATA_FILE, PROVENANCE_FILE, Paths};

const HOUR: Duration = Duration::from_hours(1);

/// Stores a 500-byte asset named by `label`, returning its bytes and hash.
fn put(store: &DiskStore, label: &str) -> (Vec<u8>, ContentHash) {
    let bytes = payload(label, 500);
    let hash = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
    (bytes, hash)
}

/// Makes an empty staging directory in `shard`, as a `put` that crashed before its rename would leave.
fn leave_staging(shard: &Path, name: &str) -> PathBuf {
    let staging = shard.join(name);
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join(PROVENANCE_FILE), b"{}").unwrap();
    fs::write(staging.join(DATA_FILE), b"half").unwrap();
    staging
}

#[test]
fn a_healthy_store_verifies_clean_and_repair_does_nothing() {
    let (dir, store) = open();
    assert_eq!(store.verify(DEFAULT_STALE_AFTER).unwrap(), VerifyReport::default(), "an empty store is clean");

    for label in ["a", "b", "c"] {
        put(&store, label);
    }
    let report = store.verify(DEFAULT_STALE_AFTER).unwrap();
    assert_eq!(report, VerifyReport { checked: 3, problems: vec![] });
    assert!(report.is_clean());

    let repaired = store.repair(DEFAULT_STALE_AFTER).unwrap();
    assert_eq!(repaired, RepairReport { checked: 3, quarantined: vec![], left_alone: vec![] });
    assert!(!dir.path().join("quarantine").exists(), "a clean store gains no quarantine folder");
}

#[test]
fn a_corrupt_object_is_quarantined_whole_and_a_new_put_stores_it_afresh() {
    let (dir, store) = open();
    let bytes = payload("corrupt", 2000);
    let hash = store.put(&bytes, full_provenance()).unwrap();
    let (_, healthy) = put(&store, "healthy");
    flip_first_byte(&store.paths(&hash).data);

    let before = files_under(dir.path());
    let report = store.verify(DEFAULT_STALE_AFTER).unwrap();
    assert_eq!((report.checked, report.problems), (2, vec![Problem::Corrupt(hash)]));
    assert_eq!(files_under(dir.path()), before, "verify only reads");

    // Why repair exists: `put` trusts what is there.
    store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
    assert_eq!(store.get(&hash), Err(StoreError::Corrupt(hash)));

    let repaired = store.repair(DEFAULT_STALE_AFTER).unwrap();
    let [moved] = repaired.quarantined.as_slice() else { panic!("exactly the corrupt object moves: {repaired:?}") };
    assert_eq!(moved.problem, Problem::Corrupt(hash));
    assert!(moved.moved_to.starts_with(dir.path().join("quarantine")));
    let kept: Provenance = serde_json::from_slice(&fs::read(moved.moved_to.join(PROVENANCE_FILE)).unwrap()).unwrap();
    assert_eq!(kept, full_provenance(), "the quarantined copy keeps its own provenance");
    assert_eq!(store.get(&hash), Err(StoreError::Missing(hash)));
    assert!(store.get(&healthy).is_ok(), "the healthy object is untouched");

    let second = Provenance { author: Some("second put".into()), ..sample_provenance(Commercial::Ok) };
    assert_eq!(store.put(&bytes, second.clone()).unwrap(), hash);
    assert_eq!(store.get(&hash).unwrap(), bytes);
    assert_eq!(store.provenance(&hash).unwrap(), second);
    assert!(store.verify(DEFAULT_STALE_AFTER).unwrap().is_clean());
}

/// Breaks one object with `damage`, then checks `verify` names it, `put` alone does not fix it, `repair` quarantines
/// it, and a fresh `put` brings the asset back with the new provenance.
fn assert_found_then_repaired(
    label: &str,
    damage: impl FnOnce(&Paths),
    is_expected: fn(&Problem, ContentHash) -> bool,
) {
    let (_dir, store) = open();
    let (bytes, hash) = put(&store, label);
    damage(&store.paths(&hash));

    let found = |store: &DiskStore| matches!(store.verify(DEFAULT_STALE_AFTER).unwrap().problems.as_slice(), [p] if is_expected(p, hash));
    assert!(found(&store), "{label}");
    store.put(&bytes, sample_provenance(Commercial::No)).unwrap();
    assert!(found(&store), "{label}: put does not repair");

    let repaired = store.repair(DEFAULT_STALE_AFTER).unwrap();
    assert_eq!(repaired.quarantined.len(), 1, "{label}");
    assert!(repaired.left_alone.is_empty(), "{label}");
    assert!(store.verify(DEFAULT_STALE_AFTER).unwrap().is_clean(), "{label}");
    assert!(!store.contains(&hash), "{label}: the broken object no longer claims the hash");

    let provenance = Provenance { author: Some(format!("re-put of {label}")), ..sample_provenance(Commercial::Ok) };
    store.put(&bytes, provenance.clone()).unwrap();
    assert_eq!(store.get(&hash).unwrap(), bytes, "{label}");
    assert_eq!(store.provenance(&hash).unwrap(), provenance, "{label}");
    assert!(store.verify(DEFAULT_STALE_AFTER).unwrap().is_clean(), "{label}");
}

#[test]
fn objects_missing_a_file_or_with_unparseable_provenance_are_found_and_repaired() {
    assert_found_then_repaired(
        "no data",
        |p| fs::remove_file(&p.data).unwrap(),
        |problem, hash| *problem == Problem::MissingData(hash),
    );
    assert_found_then_repaired(
        "no provenance",
        |p| fs::remove_file(&p.provenance).unwrap(),
        |problem, hash| *problem == Problem::MissingProvenance(hash),
    );
    assert_found_then_repaired(
        "garbled provenance",
        |p| fs::write(&p.provenance, b"{ not json").unwrap(),
        |problem, hash| matches!(problem, Problem::BadProvenance { hash: h, .. } if *h == hash),
    );
}

#[test]
fn an_object_missing_its_provenance_keeps_its_bytes_in_quarantine() {
    let (_dir, store) = open();
    let (bytes, hash) = put(&store, "no provenance");
    fs::remove_file(store.paths(&hash).provenance).unwrap();

    let repaired = store.repair(DEFAULT_STALE_AFTER).unwrap();
    let [moved] = repaired.quarantined.as_slice() else { panic!("{repaired:?}") };
    assert_eq!(fs::read(moved.moved_to.join(DATA_FILE)).unwrap(), bytes, "repair never discards bytes");
}

#[test]
fn unreadable_objects_and_strays_are_reported_but_left_alone() {
    let (dir, store) = open();
    let (_, unreadable) = put(&store, "unreadable");
    // A directory where the provenance file belongs: reading it fails with something other than "not found".
    let provenance = store.paths(&unreadable).provenance;
    fs::remove_file(&provenance).unwrap();
    fs::create_dir(&provenance).unwrap();

    let objects = dir.path().join("objects");
    let file_at_top = objects.join("README");
    let bad_shard = objects.join("zz");
    let shard = store.paths(&unreadable).shard;
    let file_in_shard = shard.join("notes.txt");
    let short_dir = shard.join("short");
    fs::write(&file_at_top, b"x").unwrap();
    fs::create_dir(&bad_shard).unwrap();
    fs::write(&file_in_shard, b"x").unwrap();
    fs::create_dir(&short_dir).unwrap();

    let report = store.verify(DEFAULT_STALE_AFTER).unwrap();
    assert_eq!(report.checked, 1);
    let mut strays = vec![file_at_top, bad_shard, file_in_shard, short_dir];
    strays.sort();
    let mut found: Vec<_> = report
        .problems
        .iter()
        .filter_map(|p| if let Problem::Stray(path) = p { Some(path.clone()) } else { None })
        .collect();
    found.sort();
    assert_eq!(found, strays);
    assert!(matches!(report.problems.iter().find(|p| !matches!(p, Problem::Stray(_))),
        Some(Problem::Unreadable { hash, reason }) if *hash == unreadable && reason.contains(&provenance.display().to_string())));

    let repaired = store.repair(DEFAULT_STALE_AFTER).unwrap();
    assert_eq!(repaired.quarantined, []);
    assert_eq!(repaired.left_alone.len(), 5);
    assert!(!dir.path().join("quarantine").exists());
    assert!(strays.iter().all(|path| path.exists()), "strays stay where they are");
}

#[test]
fn a_zero_threshold_makes_every_staging_directory_stale() {
    let (dir, store) = open();
    let (_, hash) = put(&store, "anchor");
    let staging = leave_staging(&store.paths(&hash).shard, ".tmp-9-9");

    assert!(store.verify(HOUR).unwrap().is_clean(), "a fresh staging directory is a live writer, not a problem");
    let report = store.verify(Duration::ZERO).unwrap();
    assert!(
        matches!(report.problems.as_slice(), [Problem::StaleStaging { path, .. }] if *path == staging),
        "{report:?}"
    );

    let repaired = store.repair(Duration::ZERO).unwrap();
    let [moved] = repaired.quarantined.as_slice() else { panic!("{repaired:?}") };
    assert!(!staging.exists());
    let shard_name = staging.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(moved.moved_to, dir.path().join("quarantine/repair-0000").join(format!("{shard_name}-.tmp-9-9")));
    assert_eq!(fs::read(moved.moved_to.join(DATA_FILE)).unwrap(), b"half", "even a crashed write is kept, not deleted");
    assert!(store.get(&hash).is_ok());
}

/// Sets the modification time of `path` (a file or directory) `by` into the past.
#[cfg(unix)]
fn age(path: &Path, by: Duration) {
    fs::File::open(path).unwrap().set_modified(SystemTime::now() - by).unwrap();
}

#[cfg(unix)]
#[test]
fn staging_goes_stale_only_when_nothing_in_it_has_changed() {
    let (_dir, store) = open();
    let (_, hash) = put(&store, "anchor");
    let shard = store.paths(&hash).shard;
    let long_ago = 2 * HOUR;

    let crashed = leave_staging(&shard, ".tmp-1-1");
    let writing = leave_staging(&shard, ".tmp-2-2"); // directory untouched for hours, but its data file is being written
    let fresh = leave_staging(&shard, ".tmp-3-3");
    for file in [DATA_FILE, PROVENANCE_FILE] {
        age(&crashed.join(file), long_ago);
    }
    age(&crashed, long_ago);
    age(&writing.join(PROVENANCE_FILE), long_ago);
    age(&writing, long_ago);

    let report = store.verify(HOUR).unwrap();
    assert!(
        matches!(report.problems.as_slice(), [Problem::StaleStaging { path, idle }] if *path == crashed && *idle >= long_ago),
        "{report:?}"
    );
    let repaired = store.repair(HOUR).unwrap();
    assert_eq!(repaired.quarantined.len(), 1);
    assert!(!crashed.exists() && writing.exists() && fresh.exists(), "only the crashed put's leftovers move");
}

#[test]
fn problems_come_in_a_fixed_order_and_each_repair_gets_its_own_batch() {
    let (dir, store) = open();
    let mut hashes: Vec<_> = ["x", "y", "z"].map(|label| put(&store, label).1).into();
    for hash in &hashes {
        flip_first_byte(&store.paths(hash).data);
    }
    hashes.sort_by_key(ToString::to_string);

    let expected: Vec<_> = hashes.iter().copied().map(Problem::Corrupt).collect();
    assert_eq!(store.verify(DEFAULT_STALE_AFTER).unwrap().problems, expected, "ordered by shard, then name");

    let first = store.repair(DEFAULT_STALE_AFTER).unwrap();
    assert_eq!(first.quarantined.iter().map(|q| q.problem.clone()).collect::<Vec<_>>(), expected);
    let batch = dir.path().join("quarantine/repair-0000");
    assert!(first.quarantined.iter().all(|q| q.moved_to.parent() == Some(batch.as_path())), "one run, one batch");

    assert_eq!(store.repair(DEFAULT_STALE_AFTER).unwrap().quarantined, [], "a second run has nothing to do");
    assert!(!dir.path().join("quarantine/repair-0001").exists(), "and makes no empty batch");

    let (_, again) = put(&store, "later");
    flip_first_byte(&store.paths(&again).data);
    let later = store.repair(DEFAULT_STALE_AFTER).unwrap();
    assert!(later.quarantined[0].moved_to.starts_with(dir.path().join("quarantine/repair-0001")));
}

#[test]
fn a_problem_that_vanished_before_the_move_is_skipped() {
    let (_dir, store) = open();
    let (_, hash) = put(&store, "raced");
    flip_first_byte(&store.paths(&hash).data);
    let source = store.paths(&hash).dir;
    let target = store.new_quarantine_batch().unwrap().join("elsewhere");

    // Another repair moved it between our verify and our move.
    assert!(move_aside(&source, &target).unwrap());
    assert!(!move_aside(&source, &target).unwrap());
}

#[test]
fn every_problem_explains_itself() {
    let hash = ContentHash::of(b"explained");
    let path = PathBuf::from("objects/ab/.tmp-1-1");
    let shown = [
        (Problem::Corrupt(hash), "no longer match"),
        (Problem::MissingData(hash), "data file is missing"),
        (Problem::MissingProvenance(hash), "licence gate"),
        (Problem::BadProvenance { hash, reason: "eof".into() }, "not a valid record (eof)"),
        (Problem::Unreadable { hash, reason: "read x: denied".into() }, "read x: denied"),
        (Problem::StaleStaging { path: path.clone(), idle: HOUR }, "untouched for 3600s"),
        (Problem::Stray(path), "not part of the store layout"),
    ];
    for (problem, fragment) in shown {
        assert!(problem.to_string().contains(fragment), "{problem:?} -> {problem}");
    }
}
