use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use omuzaki_store_api::{Commercial, GateError, Provenance, Store, StoreError, licence_gate};
use omuzaki_store_mock::conformance::{self, full_provenance, payload, sample_provenance};
use tempfile::TempDir;

use super::*;

pub(crate) fn open() -> (TempDir, DiskStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = DiskStore::open(dir.path()).unwrap();
    (dir, store)
}

/// Every file below `dir`, recursively.
pub(crate) fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files.sort();
    files
}

pub(crate) fn flip_first_byte(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();
}

#[test]
fn passes_the_conformance_suite() {
    let (_dir, store) = open();
    conformance::run_all(&store);
}

#[test]
fn assets_use_the_documented_layout() {
    let (dir, store) = open();
    let bytes = payload("layout", 1000);
    let hash = store.put(&bytes, full_provenance()).unwrap();

    let hex = hash.to_string();
    let asset = dir.path().join("objects").join(&hex[..2]).join(&hex[2..]);
    assert_eq!(fs::read(asset.join("data")).unwrap(), bytes);
    let on_disk: Provenance = serde_json::from_slice(&fs::read(asset.join("provenance.json")).unwrap()).unwrap();
    assert_eq!(on_disk, full_provenance());
    assert_eq!(
        files_under(&dir.path().join("objects")),
        [asset.join("committed-at"), asset.join("data"), asset.join("provenance.json")],
        "nothing else is left behind, staging directories included"
    );
    assert_eq!(fs::read_dir(asset.parent().unwrap()).unwrap().count(), 1, "the shard holds only the asset");
}

#[test]
fn identical_bytes_are_stored_once_and_the_first_provenance_stays_on_disk() {
    let (dir, store) = open();
    let bytes = payload("dedup", 4096);
    let first = Provenance { author: Some("first".into()), ..sample_provenance(Commercial::Ok) };
    let a = store.put(&bytes, first.clone()).unwrap();
    let b = store.put(&bytes, Provenance { author: Some("second".into()), ..first.clone() }).unwrap();

    assert_eq!(a, b);
    assert_eq!(
        files_under(&dir.path().join("objects")).len(),
        3,
        "one data file, its provenance and its commit marker"
    );
    assert_eq!(store.provenance(&a).unwrap(), first);
}

#[test]
fn a_flipped_byte_is_reported_as_corrupt() {
    let (_dir, store) = open();
    let victim = store.put(&payload("victim", 5000), sample_provenance(Commercial::Ok)).unwrap();
    let bystander_bytes = payload("bystander", 5000);
    let bystander = store.put(&bystander_bytes, sample_provenance(Commercial::Ok)).unwrap();

    flip_first_byte(&store.paths(&victim).data);

    assert_eq!(store.get(&victim), Err(StoreError::Corrupt(victim)));
    assert!(store.contains(&victim), "a corrupt object is still there; callers decide what to do");
    assert_eq!(store.get(&bystander).unwrap(), bystander_bytes, "other assets are unaffected");
}

#[test]
fn a_truncated_object_is_reported_as_corrupt() {
    let (_dir, store) = open();
    let hash = store.put(&payload("truncated", 5000), sample_provenance(Commercial::Ok)).unwrap();
    fs::write(store.paths(&hash).data, b"short").unwrap();
    assert_eq!(store.get(&hash), Err(StoreError::Corrupt(hash)));
}

#[test]
fn a_store_reopens_with_its_contents() {
    let (dir, store) = open();
    let bytes = payload("persist", 777);
    let hash = store.put(&bytes, full_provenance()).unwrap();
    drop(store);

    let reopened = DiskStore::open(dir.path()).unwrap();
    assert_eq!(reopened.get(&hash).unwrap(), bytes);
    assert_eq!(reopened.provenance(&hash).unwrap(), full_provenance());
}

#[test]
fn open_failures_name_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    fs::write(&file, b"x").unwrap();

    let Err(StoreError::Io(message)) = DiskStore::open(&file) else { panic!("opening below a file must fail") };
    assert!(message.contains(&file.display().to_string()), "{message}");
}

#[test]
fn put_failures_name_the_path() {
    let (_dir, store) = open();
    let bytes = payload("blocked shard", 100);
    let paths = store.paths(&ContentHash::of(&bytes));
    fs::write(&paths.shard, b"in the way").unwrap();

    let Err(StoreError::Io(message)) = store.put(&bytes, sample_provenance(Commercial::Ok)) else {
        panic!("a file where the shard directory belongs must fail the put")
    };
    assert!(message.contains(&paths.shard.display().to_string()), "{message}");
}

#[test]
fn staging_leftovers_are_invisible_and_claim_nothing() {
    let (_dir, store) = open();
    let bytes = payload("crashed", 300);
    let hash = ContentHash::of(&bytes);
    let paths = store.paths(&hash);

    // A put that died while staging: the provenance and half the data are written, the rename never happened.
    let crashed = Provenance { author: Some("crashed put".into()), ..sample_provenance(Commercial::Ok) };
    let staging = paths.shard.join(".tmp-1-1");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join(PROVENANCE_FILE), serde_json::to_vec(&crashed).unwrap()).unwrap();
    fs::write(staging.join(DATA_FILE), &bytes[..150]).unwrap();

    assert!(!store.contains(&hash));
    assert_eq!(store.get(&hash), Err(StoreError::Missing(hash)));
    assert_eq!(store.provenance(&hash), Err(StoreError::Missing(hash)));

    let retried = Provenance { author: Some("retry".into()), ..sample_provenance(Commercial::No) };
    assert_eq!(store.put(&bytes, retried.clone()).unwrap(), hash);
    assert_eq!(store.get(&hash).unwrap(), bytes);
    assert_eq!(store.provenance(&hash).unwrap(), retried, "the crashed attempt's provenance is not inherited");
}

/// Writes half the bytes, then fails like a full disk or a file-size limit (`ulimit -f`) would.
fn write_that_runs_out_of_space(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.file_name().is_some_and(|name| name == DATA_FILE) {
        fs::write(path, &bytes[..bytes.len() / 2])?;
        return Err(io::Error::from(io::ErrorKind::StorageFull));
    }
    write_file(path, bytes)
}

#[test]
fn a_put_whose_data_write_fails_leaves_nothing_and_a_retry_records_its_own_provenance() {
    let (_dir, store) = open();
    let bytes = payload("disk full", 4096);
    let hash = ContentHash::of(&bytes);
    let paths = store.paths(&hash);
    let claims_commercial_use = sample_provenance(Commercial::Ok);
    let failing = DiskStore { write: write_that_runs_out_of_space, ..store.clone() };

    // The provenance file is written before the data, so the failure lands after it: the worst case.
    let Err(StoreError::Io(message)) = failing.put(&bytes, claims_commercial_use) else {
        panic!("the failed data write must fail the put")
    };
    assert!(message.contains(&paths.dir.display().to_string()), "{message}");
    assert!(!store.contains(&hash));
    assert_eq!(store.provenance(&hash), Err(StoreError::Missing(hash)));
    assert_eq!(fs::read_dir(&paths.shard).unwrap().count(), 0, "no asset directory and no staging directory");

    let says_no = Provenance { licence: "CC-BY-NC-4.0".into(), ..sample_provenance(Commercial::No) };
    store.put(&bytes, says_no.clone()).unwrap();
    let stored = store.provenance(&hash).unwrap();
    assert_eq!(stored, says_no, "the retry's provenance is the one on record");
    assert!(matches!(licence_gate(&stored, false), Err(GateError::NonCommercial { .. })));
}

#[test]
fn a_data_file_without_provenance_is_an_error_not_a_pass() {
    let (_dir, store) = open();
    let hash = store.put(&payload("tampered", 100), sample_provenance(Commercial::Ok)).unwrap();
    let provenance = store.paths(&hash).provenance;
    fs::remove_file(&provenance).unwrap();

    let Err(StoreError::Io(message)) = store.provenance(&hash) else { panic!("missing provenance must be an error") };
    assert!(message.contains(&provenance.display().to_string()), "{message}");
}

#[test]
fn an_unparseable_provenance_is_an_error_naming_the_file() {
    let (_dir, store) = open();
    let hash = store.put(&payload("garbled", 100), sample_provenance(Commercial::Ok)).unwrap();
    let provenance = store.paths(&hash).provenance;
    fs::write(&provenance, b"{ not json").unwrap();

    let Err(StoreError::Io(message)) = store.provenance(&hash) else { panic!("garbled provenance must be an error") };
    assert!(message.contains(&provenance.display().to_string()), "{message}");
}

#[test]
fn racing_puts_leave_no_debris_and_every_asset_is_complete() {
    let (dir, store) = open();
    conformance::concurrent_puts_agree(&store);

    let files = files_under(&dir.path().join("objects"));
    assert!(files.len() >= 2, "the race stored something");
    for file in &files {
        assert!(
            !file.components().any(|part| part.as_os_str().to_string_lossy().starts_with(".tmp-")),
            "leftover staging file {}",
            file.display()
        );
        let name = file.file_name().unwrap();
        let sibling = if name == DATA_FILE { PROVENANCE_FILE } else { DATA_FILE };
        assert!(file.with_file_name(sibling).is_file(), "{} has no {sibling} beside it", file.display());
    }
}

#[test]
fn the_loser_of_a_commit_race_changes_nothing_and_leaves_no_staging_directory() {
    let (_dir, store) = open();
    let bytes = payload("lost race", 500);
    let first = Provenance { author: Some("winner".into()), ..sample_provenance(Commercial::Ok) };
    let hash = store.put(&bytes, first.clone()).unwrap();
    let paths = store.paths(&hash);

    // A second writer that passed the "already stored?" check before the winner committed: it stages a different
    // provenance and loses at the rename.
    let loser = serde_json::to_vec(&Provenance { author: Some("loser".into()), ..first.clone() }).unwrap();
    let created = publish_dir(&paths.dir, &[(PROVENANCE_FILE, &loser), (DATA_FILE, &bytes)], write_file).unwrap();

    assert!(!created, "the existing asset must win");
    assert_eq!(store.provenance(&hash).unwrap(), first);
    assert_eq!(store.get(&hash).unwrap(), bytes);
    assert_eq!(fs::read_dir(&paths.shard).unwrap().count(), 1, "the loser's staging directory is gone");
}
