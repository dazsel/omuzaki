//! A saved project under the system `git`: `.omuzaki/` is never committed, and an edit, a save or a move shows up as
//! exactly the lines and files that changed.

// Helpers outside `#[test]` functions may unwrap too: a failed setup should just panic.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::{change, p, read, snapshot, write};
use omuzaki_doc::{Project, ProjectLock};
use omuzaki_doc_api::{DocWrite, Patch};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The one character every test starts from: a face with a number to edit, and a variant.
fn mia() -> Value {
    json!({"face": {"smile": 5, "name": "Mia"}, "variants": {"game": {"scale": 2}}})
}

/// Runs the system `git` in `dir` with the runner's configuration kept out: an empty global config, no system config
/// and a fixed identity. Panics with git's error output if it fails.
fn git(dir: &Path, args: &[&str]) -> String {
    // Beside the project, not in it, so the snapshots of the project never see it.
    let config = dir.parent().unwrap().join("empty-gitconfig");
    fs::write(&config, "").unwrap();
    let output = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap_or_else(|e| panic!("the system `git` program could not be run ({e}); these tests need it on PATH"));
    assert!(output.status.success(), "git {args:?} failed:\n{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

/// Puts the project in a new git repository and commits everything in it.
fn init_and_commit(dir: &Path, message: &str) {
    git(dir, &["init", "-q"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// The `.toml` files under `dir` (relative paths), with their bytes.
fn toml_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    snapshot(dir).into_iter().filter(|(rel, _)| Path::new(rel).extension().is_some_and(|ext| ext == "toml")).collect()
}

#[test]
fn the_lock_folder_is_ignored_and_stays_out_of_the_tree() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("show.omuzaki");
    Project::create(&dir, "Show").unwrap();
    let _lock = ProjectLock::acquire(&dir).unwrap();
    assert_eq!(read(&dir, ".omuzaki/.gitignore"), "*\n");

    let reopened = Project::open(&dir).unwrap();
    let keys = reopened.root().as_object().unwrap().keys();
    assert!(keys.clone().all(|key| !key.starts_with('.')), "a dot-folder reached the tree: {keys:?}");
}

#[test]
fn an_existing_omuzaki_gitignore_is_left_byte_for_byte() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), ".omuzaki/.gitignore", "custom\r\n# kept\n");
    let _lock = ProjectLock::acquire(tmp.path()).unwrap();
    assert_eq!(read(tmp.path(), ".omuzaki/.gitignore"), "custom\r\n# kept\n");
}

#[test]
fn a_saved_project_commits_cleanly_through_git() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("show.omuzaki");
    let mut project = Project::create(&dir, "Show").unwrap();
    let _lock = ProjectLock::acquire(&dir).unwrap();
    project.apply(&change("characters", None, Some(json!({"mia": mia()})))).unwrap();
    project.save().unwrap();

    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    let status = git(&dir, &["status", "--porcelain"]);
    assert!(!status.contains(".omuzaki"), "the lock folder is in git's view:\n{status}");
    git(&dir, &["commit", "-q", "-m", "first save"]);
    let tracked = git(&dir, &["ls-files"]);
    assert!(!tracked.lines().any(|f| f.starts_with(".omuzaki/")), "tracked:\n{tracked}");
    assert_eq!(read(&dir, ".omuzaki/.gitignore"), "*\n");

    // One parameter changes: one file, one line in and one out.
    project.apply(&change("characters/mia/face/smile", Some(json!(5)), Some(json!(6)))).unwrap();
    project.save().unwrap();
    assert_eq!(git(&dir, &["diff", "--numstat"]), "1\t1\tcharacters/mia/face.toml\n");
    git(&dir, &["commit", "-q", "-am", "smile"]);

    // A save with nothing changed writes nothing.
    project.save().unwrap();
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");

    // Moving a character folder is a rename of every file in it, with the same bytes.
    let mia_now = project.root()["characters"]["mia"].clone();
    let moved = Patch::new().with(p("characters/mia"), Some(mia_now.clone()), None).with(
        p("characters/mira"),
        None,
        Some(mia_now),
    );
    project.apply(&moved).unwrap();
    project.save().unwrap();
    git(&dir, &["add", "-A"]);
    let renames = git(&dir, &["diff", "--cached", "--name-status", "-M"]);
    assert!(!renames.is_empty(), "the move changed nothing in git");
    assert!(renames.lines().all(|line| line.starts_with("R100\t")), "not a pure rename:\n{renames}");
}

#[test]
fn a_crlf_checkout_saves_with_one_line_changed_under_autocrlf() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("show.omuzaki");
    let mut project = Project::create(&dir, "Show").unwrap();
    let _lock = ProjectLock::acquire(&dir).unwrap();
    project.apply(&change("characters", None, Some(json!({"mia": mia()})))).unwrap();
    project.save().unwrap();
    init_and_commit(&dir, "first save");

    // What `core.autocrlf=true` leaves on disk for a Windows checkout: every text file with CRLF line endings.
    for (rel, bytes) in toml_files(&dir) {
        let text = String::from_utf8(bytes).unwrap();
        write(&dir, &rel, &text.replace('\n', "\r\n"));
    }

    let mut reopened = Project::open(&dir).unwrap();
    reopened.apply(&change("characters/mia/face/smile", Some(json!(5)), Some(json!(6)))).unwrap();
    reopened.save().unwrap();

    for (rel, bytes) in toml_files(&dir) {
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.replace("\r\n", "").contains('\n'), "{rel} lost its CRLF line endings");
    }
    assert_eq!(git(&dir, &["-c", "core.autocrlf=true", "diff", "--numstat"]), "1\t1\tcharacters/mia/face.toml\n");
}
