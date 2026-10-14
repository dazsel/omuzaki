//! The store never writes through a symbolic link, or over one.
#![cfg(unix)]
// Helpers outside `#[test]` functions may unwrap too: a failed setup should just panic.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;

use common::{change, read, snapshot, write};
use omuzaki_doc::{Project, StoreError};
use omuzaki_doc_api::DocWrite;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn a_symlinked_folder_is_not_written_through() {
    let tmp = TempDir::new().unwrap();
    let outer = tmp.path();
    write(outer, "elsewhere/keep.txt", "x");
    let dir = outer.join("proj");
    write(&dir, "project.toml", "name = \"x\"\nschema = 1\n");
    symlink(outer.join("elsewhere"), dir.join("characters")).unwrap();
    let elsewhere = snapshot(&outer.join("elsewhere"));

    let mut project = Project::open(&dir).unwrap();
    project.apply(&change("characters", None, Some(json!({"mia": {"face": {"a": 1}}})))).unwrap();
    let err = project.save().unwrap_err();
    assert!(matches!(&err, StoreError::Symlink { path } if path.ends_with("characters")), "{err}");
    assert!(err.to_string().contains("never writes through"), "{err}");
    assert_eq!(snapshot(&outer.join("elsewhere")), elsewhere, "save wrote through a symlink out of the project");
    assert!(project.is_dirty(), "the edit is still waiting");

    // With the link replaced by a real folder the same save goes through.
    fs::remove_file(dir.join("characters")).unwrap();
    project.save().unwrap();
    assert_eq!(read(&dir, "characters/mia/face.toml"), "a = 1\n");
    assert_eq!(snapshot(&outer.join("elsewhere")), elsewhere);
}

#[test]
fn a_symlinked_file_is_not_replaced_or_written_through() {
    let tmp = TempDir::new().unwrap();
    let outer = tmp.path();
    write(outer, "shared/face.toml", "a = 1\n");
    let dir = outer.join("proj");
    write(&dir, "project.toml", "name = \"x\"\nschema = 1\n");
    write(&dir, "characters/mia/rig.toml", "b = 1\n");
    symlink(outer.join("shared/face.toml"), dir.join("characters/mia/face.toml")).unwrap();

    let mut project = Project::open(&dir).unwrap();
    // The link is not in the tree, so a `face` entry is new as far as the store knows.
    project.apply(&change("characters/mia/face", None, Some(json!({"a": 2})))).unwrap();
    let err = project.save().unwrap_err();
    assert!(matches!(err, StoreError::Symlink { .. }), "{err}");
    assert_eq!(read(outer, "shared/face.toml"), "a = 1\n");
    assert!(fs::symlink_metadata(dir.join("characters/mia/face.toml")).unwrap().file_type().is_symlink());
}

#[test]
fn a_project_opened_through_a_link_to_its_folder_still_saves() {
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    write(&real, "project.toml", "name = \"x\"\nschema = 1\n");
    let shortcut = tmp.path().join("shortcut");
    symlink(&real, &shortcut).unwrap();

    let mut project = Project::open(&shortcut).unwrap();
    project.apply(&change("scenes", None, Some(json!({"s": {"stage": "x"}})))).unwrap();
    project.save().unwrap();
    assert_eq!(read(&real, "scenes/s.toml"), "stage = \"x\"\n");
}
