//! A project remembers each file's text and hash, renders saves from memory, and refuses to overwrite a file that
//! changed on disk.

// Helpers outside `#[test]` functions may unwrap too: a failed setup should just panic.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::{change, read, snapshot, write};
use omuzaki_doc::{Project, StoreError};
use omuzaki_doc_api::DocWrite;
use omuzaki_store_api::ContentHash;
use serde_json::json;
use tempfile::TempDir;

const FACE: &str = "# hand-written\nprofile = \"round\"  # keep me\nblend = 0.5\n";

fn folder() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "project.toml", "name = \"x\"\nschema = 1\n");
    write(tmp.path(), "characters/mia/face.toml", FACE);
    write(tmp.path(), "characters/mia/rig.toml", "template = \"humanoid\"\n");
    tmp
}

fn edit_blend(project: &mut Project, from: f64, to: f64) {
    project.apply(&change("characters/mia/face/blend", Some(json!(from)), Some(json!(to)))).unwrap();
}

#[test]
fn hashes_are_those_of_the_bytes_read() {
    let tmp = folder();
    let project = Project::open(tmp.path()).unwrap();
    let hashes = project.file_hashes();
    assert_eq!(hashes.len(), 3);
    assert_eq!(hashes[&PathBuf::from("characters/mia/face.toml")], ContentHash::of(FACE.as_bytes()));
}

#[test]
fn rendered_files_equal_what_save_writes_and_need_no_disk() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    let before = snapshot(tmp.path());

    let rendered = project.rendered_files().unwrap();
    assert_eq!(snapshot(tmp.path()), before, "rendering must not touch the disk");
    let changed: Vec<_> = rendered.iter().filter(|f| f.changed).collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].path, PathBuf::from("characters/mia/face.toml"));
    assert!(changed[0].text.contains("# hand-written") && changed[0].text.contains("0.9"));
    assert_eq!(changed[0].hash, ContentHash::of(changed[0].text.as_bytes()));

    let report = project.save().unwrap();
    assert_eq!(report.written, 1);
    for file in &rendered {
        assert_eq!(read(tmp.path(), &file.path.to_string_lossy()), file.text);
    }
    assert!(project.rendered_files().unwrap().iter().all(|f| !f.changed));
    assert_eq!(project.file_hashes()[&PathBuf::from("characters/mia/face.toml")], changed[0].hash);
}

#[test]
fn a_new_file_renders_as_changed_and_a_deleted_one_is_gone() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    let rig = json!({"template": "humanoid"});
    project.apply(&change("characters/mia/rig", Some(rig), None)).unwrap();
    project.apply(&change("characters/mia/look", None, Some(json!({"outline": 1})))).unwrap();
    let rendered = project.rendered_files().unwrap();
    let paths: Vec<_> = rendered.iter().map(|f| f.path.to_string_lossy().into_owned()).collect();
    assert_eq!(paths, ["characters/mia/face.toml", "characters/mia/look.toml", "project.toml"]);
    assert!(rendered.iter().find(|f| f.path.ends_with("look.toml")).unwrap().changed);
    project.save().unwrap();
    assert_eq!(project.file_hashes().len(), 3);
}

#[test]
fn a_file_edited_between_open_and_save_is_refused_and_untouched_but_the_others_are_written() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    project.apply(&change("characters/mia/rig/template", Some(json!("humanoid")), Some(json!("quad")))).unwrap();
    write(tmp.path(), "characters/mia/face.toml", "profile = \"edited by hand\"\n");

    let err = project.save().unwrap_err();
    assert!(matches!(&err, StoreError::ChangedOnDisk { path } if path.ends_with("face.toml")), "{err}");
    assert_eq!(read(tmp.path(), "characters/mia/face.toml"), "profile = \"edited by hand\"\n");
    assert_eq!(read(tmp.path(), "characters/mia/rig.toml"), "template = \"quad\"\n");
    assert!(project.is_dirty(), "the refused file is still owed");
}

#[test]
fn project_toml_waits_when_another_file_was_refused() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    project.apply(&change("project/name", Some(json!("x")), Some(json!("y")))).unwrap();
    write(tmp.path(), "characters/mia/face.toml", "other = 1\n");
    assert!(matches!(project.save(), Err(StoreError::ChangedOnDisk { .. })));
    assert_eq!(read(tmp.path(), "project.toml"), "name = \"x\"\nschema = 1\n");
}

#[test]
fn a_file_created_where_the_project_has_none_is_not_overwritten() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    project.apply(&change("characters/mia/look", None, Some(json!({"outline": 1})))).unwrap();
    write(tmp.path(), "characters/mia/look.toml", "theirs = true\n");
    assert!(matches!(project.save(), Err(StoreError::ChangedOnDisk { .. })));
    assert_eq!(read(tmp.path(), "characters/mia/look.toml"), "theirs = true\n");
}

#[test]
fn a_second_save_after_our_own_write_is_not_a_conflict() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    project.save().unwrap();
    edit_blend(&mut project, 0.9, 0.2);
    assert_eq!(project.save().unwrap().written, 1);
    assert!(read(tmp.path(), "characters/mia/face.toml").contains("# keep me"));
}

#[test]
fn a_file_removed_since_it_was_read_is_written_again() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    std::fs::remove_file(tmp.path().join("characters/mia/face.toml")).unwrap();
    project.save().unwrap();
    assert!(read(tmp.path(), "characters/mia/face.toml").contains("0.9"));
}

#[test]
fn a_clone_is_independent() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    let mut clone = project.clone();
    edit_blend(&mut clone, 0.5, 0.1);
    assert!(clone.is_dirty() && !project.is_dirty());
    assert_ne!(clone.rendered_files().unwrap(), project.rendered_files().unwrap());
    edit_blend(&mut project, 0.5, 0.7);
    assert!(clone.rendered_files().unwrap().iter().any(|f| f.text.contains("0.1")));
    assert!(project.rendered_files().unwrap().iter().all(|f| !f.text.contains("0.1")));
}

#[test]
fn a_file_changed_into_broken_toml_is_reported_as_a_syntax_error() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    write(tmp.path(), "characters/mia/face.toml", "blend = \n");
    assert!(matches!(project.save(), Err(StoreError::Syntax { .. })));
    assert_eq!(read(tmp.path(), "characters/mia/face.toml"), "blend = \n");
}

#[test]
fn a_replaced_file_is_retained_in_the_replaced_folder_until_it_is_old() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    project.save().unwrap();
    // The displaced bytes were the last-known text, kept (with their hash) in case an editor still writes to them.
    let files: Vec<_> = snapshot(tmp.path())
        .into_iter()
        .map(|(name, _)| name)
        .filter(|n| n.starts_with(".omuzaki/replaced/"))
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].contains("face.toml.") && files[0].contains(".retained-"), "{files:?}");
}

/// The destination never goes missing during a replacement, so a reader opening the project while it is saved always
/// finds `project.toml` (the old move-aside design briefly reported `NotAProject`).
#[test]
fn project_toml_stays_openable_while_it_is_saved() {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Stops the reader when dropped, so a panic in the writer can't leave the scope waiting for it.
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    let tmp = folder();
    let dir = tmp.path().to_path_buf();
    let mut project = Project::open(&dir).unwrap();
    let done = AtomicBool::new(false);
    let start = Barrier::new(2);
    std::thread::scope(|s| {
        s.spawn(|| {
            start.wait();
            while !done.load(Ordering::Relaxed) {
                Project::open(&dir).unwrap();
            }
        });
        let _stop = Stop(&done);
        start.wait();
        let mut name = "x".to_owned();
        for i in 0..200 {
            let next = format!("n{i}");
            project.apply(&change("project/name", Some(json!(name)), Some(json!(next)))).unwrap();
            project.save().unwrap();
            name = next;
        }
    });
}
