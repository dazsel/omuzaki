//! `omuzaki store verify` and `omuzaki store repair` as a script sees them: exit codes, stdout and stderr, on a real store on
//! disk. The acceptance check of is the first test: corrupt an object, verify fails, repair quarantines it, and a
//! second verify is clean.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::path::Path;
use std::process::{Command, Output};

use omuzaki_store::DiskStore;
use omuzaki_store_api::{Commercial, Provenance, Source, Store};

fn omuzaki(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omuzaki")).args(args).output().expect("run omuzaki")
}

/// `omuzaki store <action> --store <dir> <extra>`.
fn store_cmd(action: &str, dir: &Path, extra: &[&str]) -> Output {
    let dir = dir.to_str().expect("utf-8 path");
    omuzaki(&[&["store", action, "--store", dir][..], extra].concat())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

fn provenance() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// A store with two assets, one of them then corrupted on disk. Returns the folder and the corrupt asset's hash.
fn corrupted_store(root: &Path) -> (std::path::PathBuf, omuzaki_store_api::ContentHash) {
    let dir = root.join("store");
    let store = DiskStore::open(&dir).expect("open store");
    let hash = store.put(b"a render", provenance()).expect("put");
    store.put(b"a second asset", provenance()).expect("put");
    let hex = hash.to_string();
    let (shard, rest) = hex.split_at(2);
    std::fs::write(dir.join("objects").join(shard).join(rest).join("data"), b"tampered").expect("corrupt");
    (dir, hash)
}

#[test]
fn a_corrupt_object_fails_verify_is_quarantined_by_repair_and_then_verifies_clean() {
    let root = tempfile::tempdir().expect("tempdir");
    let (dir, hash) = corrupted_store(root.path());

    let first = store_cmd("verify", &dir, &[]);
    assert_eq!(first.status.code(), Some(11), "stderr: {}", text(&first.stderr));
    assert!(text(&first.stdout).contains(&format!("asset {hash}: its bytes no longer match the hash")));
    assert!(text(&first.stdout).starts_with("checked 2 objects: 1 problem\n"), "{}", text(&first.stdout));

    let repaired = store_cmd("repair", &dir, &[]);
    assert_eq!(repaired.status.code(), Some(0), "stderr: {}", text(&repaired.stderr));
    let report = text(&repaired.stdout);
    assert!(report.contains("moved to quarantine:"), "{report}");
    assert!(report.contains("-> "), "{report}");
    assert!(report.contains("is yours to delete once you are sure"), "{report}");
    assert!(!report.contains("left alone"), "{report}");

    let second = store_cmd("verify", &dir, &[]);
    assert_eq!(second.status.code(), Some(0), "stdout: {}", text(&second.stdout));
    assert_eq!(text(&second.stdout), "checked 1 object: the store is clean\n");
}

#[test]
fn verify_json_names_each_problem_by_kind() {
    let root = tempfile::tempdir().expect("tempdir");
    let (dir, _) = corrupted_store(root.path());

    let out = store_cmd("verify", &dir, &["--json"]);
    assert_eq!(out.status.code(), Some(11));
    let body: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("stdout is JSON");
    assert_eq!(body["checked"], 2);
    assert_eq!(body["clean"], false);
    assert_eq!(body["problems"][0]["kind"], "corrupt");
    assert!(body["problems"][0]["text"].as_str().expect("text").contains("no longer match the hash"));
}

#[test]
fn repair_json_says_where_each_damaged_object_went() {
    let root = tempfile::tempdir().expect("tempdir");
    let (dir, _) = corrupted_store(root.path());

    let out = store_cmd("repair", &dir, &["--json"]);
    assert_eq!(out.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("stdout is JSON");
    assert_eq!(body["checked"], 2);
    assert_eq!(body["quarantined"][0]["kind"], "corrupt");
    let moved = body["quarantined"][0]["moved_to"].as_str().expect("moved_to");
    assert!(Path::new(moved).starts_with(dir.join("quarantine")), "{moved}");
    assert_eq!(body["left_alone"], serde_json::json!([]));
}

#[test]
fn a_clean_store_verifies_and_repairs_with_exit_0() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("store");
    DiskStore::open(&dir).expect("open store").put(b"fine", provenance()).expect("put");

    assert_eq!(store_cmd("verify", &dir, &[]).status.code(), Some(0));
    let repaired = store_cmd("repair", &dir, &[]);
    assert_eq!(repaired.status.code(), Some(0));
    assert_eq!(text(&repaired.stdout), "checked 1 object\nnothing to repair: the store is clean\n");
}

#[test]
fn a_folder_without_objects_is_not_a_store_and_is_not_created_into_one() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("typo");
    std::fs::create_dir(&dir).expect("make folder");

    let out = store_cmd("verify", &dir, &[]);
    assert_eq!(out.status.code(), Some(4), "stderr: {}", text(&out.stderr));
    assert!(text(&out.stderr).contains("error: not_found: no store at"), "{}", text(&out.stderr));
    assert!(!dir.join("objects").exists(), "verify must not create objects/");
}

#[test]
fn stale_after_is_read_and_a_bare_number_is_a_usage_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("store");
    DiskStore::open(&dir).expect("open store");

    assert_eq!(store_cmd("verify", &dir, &["--stale-after", "30m"]).status.code(), Some(0));
    let bare = store_cmd("verify", &dir, &["--stale-after", "30"]);
    assert_eq!(bare.status.code(), Some(2));
    assert_eq!(text(&bare.stdout), "");
}

#[test]
fn store_without_its_folder_is_a_usage_error() {
    assert_eq!(omuzaki(&["store", "verify"]).status.code(), Some(2));
}

#[test]
fn a_stray_file_in_objects_is_left_alone_and_repair_exits_11() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("store");
    DiskStore::open(&dir).expect("open store").put(b"fine", provenance()).expect("put");
    let stray = dir.join("objects").join("notes.txt");
    std::fs::write(&stray, b"not ours").expect("write stray");

    let text_out = store_cmd("repair", &dir, &[]);
    assert_eq!(text_out.status.code(), Some(11), "stderr: {}", text(&text_out.stderr));
    let report = text(&text_out.stdout);
    assert!(report.contains("left alone (needs a person):"), "{report}");
    assert!(report.contains("notes.txt: not part of the store layout"), "{report}");
    assert!(!report.contains("moved to quarantine"), "{report}");
    assert_eq!(std::fs::read(&stray).expect("stray still there"), b"not ours");

    let out = store_cmd("repair", &dir, &["--json"]);
    assert_eq!(out.status.code(), Some(11));
    let body: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("stdout is JSON");
    assert_eq!(body["quarantined"], serde_json::json!([]));
    assert_eq!(body["left_alone"][0]["kind"], "stray");
}
