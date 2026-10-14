//! Saves that stop part way (a crash, a full disk): what is left on disk must stay openable and correct.
//!
//! A failing write is simulated by making a folder read-only. These tests skip themselves when that doesn't stop
//! writes (running as root).
#![cfg(unix)]
// Helpers outside `#[test]` functions may unwrap too, and the test migration has the fallible signature of real ones.
#![allow(clippy::unwrap_used, clippy::unnecessary_wraps)]

mod common;

use std::fs;

use common::{change, p, read, readonly_is_enforced, with_readonly, write};
use omuzaki_doc::{MigrationError, Project, register_migration};
use omuzaki_doc_api::{DocRead, DocWrite};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Schema 2 renames `old` to `new` in every file under `zones` and `areas`. Safe to run twice, as every migration
/// must be.
fn rename(project: &mut Value) -> Result<(), MigrationError> {
    for folder in ["zones", "areas"] {
        let Some(files) = project.get_mut(folder).and_then(Value::as_object_mut) else { continue };
        for file in files.values_mut() {
            if let Some(v) = file.as_object_mut().and_then(|f| f.remove("old")) {
                file["new"] = v;
            }
        }
    }
    Ok(())
}
register_migration!(2, "rename_old_to_new", rename);

const FACE: &str = "# my face notes\nborn = 2026-10-11 # keep\nprofile = \"round\" # keep\n";

#[test]
fn an_interrupted_migration_save_is_redone_on_the_next_open() {
    if !readonly_is_enforced() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "project.toml", "name = \"x\"\nschema = 1\n");
    write(dir, "zones/z.toml", "old = 1\n");
    write(dir, "zones/y.toml", "old = 2\n");
    let mut project = Project::open(dir).unwrap();
    assert_eq!(project.upgraded_from(), Some(1));

    // `project.toml` sorts before `zones`; if it were written in path order it would already say schema 2 here.
    let saved = with_readonly(&dir.join("zones"), || project.save());
    assert!(saved.is_err());
    assert_eq!(read(dir, "project.toml"), "name = \"x\"\nschema = 1\n", "the schema is recorded last");
    drop(project);

    let reopened = Project::open(dir).unwrap();
    assert_eq!(reopened.upgraded_from(), Some(1), "the migration must run again");
    assert_eq!(reopened.get(&p("zones/z")), Some(&json!({"new": 1})));
    assert_eq!(reopened.get(&p("zones/y")), Some(&json!({"new": 2})));
}

#[test]
fn a_migration_save_that_stops_after_some_files_reopens_with_the_rest_migrated() {
    if !readonly_is_enforced() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "project.toml", "name = \"x\"\nschema = 1\n");
    write(dir, "areas/a.toml", "old = 1\n");
    write(dir, "zones/z.toml", "old = 2\n");
    let mut project = Project::open(dir).unwrap();
    // `areas` is written, then `zones` fails: a mix of migrated and old files, with the old schema still recorded.
    assert!(with_readonly(&dir.join("zones"), || project.save()).is_err());
    assert_eq!(read(dir, "areas/a.toml"), "new = 1\n");
    assert_eq!(read(dir, "zones/z.toml"), "old = 2\n");
    assert_eq!(read(dir, "project.toml"), "name = \"x\"\nschema = 1\n");
    drop(project);

    let reopened = Project::open(dir).unwrap();
    assert_eq!(reopened.upgraded_from(), Some(1));
    assert_eq!(reopened.get(&p("areas/a")), Some(&json!({"new": 1})), "already migrated, and left so");
    assert_eq!(reopened.get(&p("zones/z")), Some(&json!({"new": 2})));
}

#[test]
fn a_failed_save_is_retried_to_completion() {
    if !readonly_is_enforced() {
        return;
    }
    // The everyday flow: a character with an empty `variants` gets its first variant.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "project.toml", "name = \"x\"\nschema = 2\n");
    write(dir, "characters/mia/face.toml", FACE);
    write(dir, "characters/mia/variants.toml", "");
    fs::create_dir_all(dir.join("characters/mia/variants")).unwrap(); // so only the delete needs the locked folder
    let mut project = Project::open(dir).unwrap();
    project.apply(&change("characters/mia/variants/game", None, Some(json!({"scale": 2})))).unwrap();
    assert!(with_readonly(&dir.join("characters/mia"), || project.save()).is_err());

    assert_eq!(project.save().unwrap().removed, 1, "the retry finishes the job");
    let reopened = Project::open(dir).unwrap();
    assert_eq!(reopened.root(), project.root());
    assert_eq!(read(dir, "characters/mia/face.toml"), FACE, "files that did not change are untouched");
}

/// Known gap: if the process dies instead of retrying, the empty `variants.toml` and the new
/// `variants/` folder are both on disk and the project does not open, though nothing in the empty file can be lost.
#[test]
#[ignore = "known gap: recovery does not yet clear an interrupted layout-changing save"]
fn the_first_variant_flow_after_the_process_dies() {
    if !readonly_is_enforced() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "project.toml", "name = \"x\"\nschema = 2\n");
    write(dir, "characters/mia/face.toml", FACE);
    write(dir, "characters/mia/variants.toml", "");
    fs::create_dir_all(dir.join("characters/mia/variants")).unwrap();
    let mut project = Project::open(dir).unwrap();
    project.apply(&change("characters/mia/variants/game", None, Some(json!({"scale": 2})))).unwrap();
    assert!(with_readonly(&dir.join("characters/mia"), || project.save()).is_err());
    drop(project);
    let reopened = Project::open(dir);
    assert!(reopened.is_ok(), "{:?}", reopened.err());
}
