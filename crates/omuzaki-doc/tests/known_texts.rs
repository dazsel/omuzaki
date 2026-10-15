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
fn a_replaced_file_is_kept_in_the_replaced_folder_under_its_hash() {
    let tmp = folder();
    let before = read(tmp.path(), "characters/mia/face.toml");
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    project.save().unwrap();
    // The displaced file stays (an editor may still hold it open), named with the hash it had: no copy of it.
    let kept: Vec<_> =
        snapshot(tmp.path()).into_iter().filter(|(name, _)| name.starts_with(".omuzaki/replaced/")).collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let (name, bytes) = &kept[0];
    assert!(name.starts_with(".omuzaki/replaced/characters/mia/face.toml."), "{name}");
    assert!(name.ends_with(&format!(".{}.kept", ContentHash::of(before.as_bytes()))), "{name}");
    assert_eq!(bytes, before.as_bytes());
}

#[test]
fn the_first_save_prunes_old_unchanged_entries_and_keeps_changed_ones() {
    let tmp = folder();
    let hash = ContentHash::of(b"old = 1\n");
    // Kept long ago (the time in the name is 1970): one untouched, one written to since.
    write(tmp.path(), &format!(".omuzaki/replaced/x.toml.1-1-0.{hash}.kept"), "old = 1\n");
    write(tmp.path(), &format!(".omuzaki/replaced/y.toml.1-1-1.{hash}.kept"), "old = 1\nedited = 2\n");
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    project.save().unwrap();
    // Which files the entries left are of: the face this save replaced, and the changed one.
    let kept: Vec<_> = snapshot(tmp.path())
        .into_iter()
        .filter_map(|(name, _)| Some(name.strip_prefix(".omuzaki/replaced/")?.split(".toml.").next()?.to_owned()))
        .collect();
    assert_eq!(kept, ["characters/mia/face", "y"]);
}

/// The destination never goes missing during a replacement, so a reader opening the project while it is saved always
/// finds `project.toml` (the old move-aside design briefly reported `NotAProject`).
#[test]
fn project_toml_stays_openable_while_it_is_saved() {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
    let attempts = AtomicUsize::new(0);
    // The writer starts only once the reader has opened the project once, so the test never runs without a reader.
    let started = Barrier::new(2);
    std::thread::scope(|s| {
        s.spawn(|| {
            loop {
                Project::open(&dir).unwrap();
                if attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                    started.wait();
                }
                if done.load(Ordering::Relaxed) {
                    break;
                }
            }
        });
        let _stop = Stop(&done);
        started.wait();
        let mut name = "x".to_owned();
        for i in 0..200 {
            let next = format!("n{i}");
            project.apply(&change("project/name", Some(json!(name)), Some(json!(next)))).unwrap();
            project.save().unwrap();
            name = next;
        }
    });
    assert!(attempts.load(Ordering::Relaxed) > 0);
}

fn remove_rig(project: &mut Project) {
    project.apply(&change("characters/mia/rig", Some(json!({"template": "humanoid"})), None)).unwrap();
}

#[test]
fn a_file_edited_outside_is_not_deleted_by_a_patch_that_removes_it() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    remove_rig(&mut project);
    write(tmp.path(), "characters/mia/rig.toml", "template = \"humanoid\"\nextra = 1\n");
    let err = project.save().unwrap_err();
    assert!(matches!(&err, StoreError::ChangedOnDisk { path } if path.ends_with("rig.toml")), "{err}");
    assert_eq!(read(tmp.path(), "characters/mia/rig.toml"), "template = \"humanoid\"\nextra = 1\n");
    assert!(project.is_dirty(), "the deletion is still waiting");
    assert!(project.file_hashes().contains_key(&PathBuf::from("characters/mia/rig.toml")), "the baseline is kept");
}

#[test]
fn an_untouched_file_is_deleted_and_kept_under_the_replaced_folder() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    remove_rig(&mut project);
    assert_eq!(project.save().unwrap().removed, 1);
    assert!(!tmp.path().join("characters/mia/rig.toml").exists());
    assert!(!project.is_dirty());
    let kept = std::fs::read_dir(tmp.path().join(".omuzaki/replaced/characters/mia")).unwrap().count();
    assert_eq!(kept, 1);
}

#[test]
fn an_edit_undone_before_saving_still_rewrites_a_file_deleted_meanwhile() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    edit_blend(&mut project, 0.9, 0.5);
    std::fs::remove_file(tmp.path().join("characters/mia/face.toml")).unwrap();
    assert_eq!(project.save().unwrap().written, 1);
    assert_eq!(read(tmp.path(), "characters/mia/face.toml"), FACE);
    assert!(!project.is_dirty());
}

/// An undone edit whose file (or a folder above it) was replaced by a symbolic link is never marked clean: the link is
/// not followed, the save reports it, and the edit stays owed. Covers a link to identical bytes, a broken link, and a
/// folder swapped for a link to a copy of itself.
#[cfg(unix)]
#[test]
fn an_edit_undone_before_saving_is_not_marked_clean_through_a_symbolic_link() {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    /// Replaces something in the project folder (first) with a link into the outside folder (second).
    type Swap = fn(&Path, &Path);
    let cases: [(&str, Swap); 3] = [
        ("a link to identical bytes", |dir, outside| {
            write(outside, "face.toml", FACE);
            std::fs::remove_file(dir.join("characters/mia/face.toml")).unwrap();
            symlink(outside.join("face.toml"), dir.join("characters/mia/face.toml")).unwrap();
        }),
        ("a broken link", |dir, outside| {
            std::fs::remove_file(dir.join("characters/mia/face.toml")).unwrap();
            symlink(outside.join("missing.toml"), dir.join("characters/mia/face.toml")).unwrap();
        }),
        ("a folder swapped for a link", |dir, outside| {
            write(outside, "mia/face.toml", FACE);
            write(outside, "mia/rig.toml", "template = \"humanoid\"\n");
            std::fs::rename(dir.join("characters/mia"), dir.join("characters/.mia-moved")).unwrap();
            symlink(outside.join("mia"), dir.join("characters/mia")).unwrap();
        }),
    ];
    for (name, swap) in cases {
        let tmp = folder();
        let outside = TempDir::new().unwrap();
        let mut project = Project::open(tmp.path()).unwrap();
        edit_blend(&mut project, 0.5, 0.9);
        edit_blend(&mut project, 0.9, 0.5);
        swap(tmp.path(), outside.path());
        let err = project.save().unwrap_err();
        assert!(matches!(err, StoreError::Symlink { .. }), "{name}: {err}");
        assert!(project.is_dirty(), "{name}");
        // Nothing was written through the link.
        assert!(!outside.path().join("mia/.face.toml.tmp").exists(), "{name}");
        if let Ok(text) = std::fs::read_to_string(outside.path().join("face.toml")) {
            assert_eq!(text, FACE, "{name}");
        }
    }
}

#[test]
fn an_edit_undone_before_saving_does_not_hide_an_external_change() {
    let tmp = folder();
    let mut project = Project::open(tmp.path()).unwrap();
    edit_blend(&mut project, 0.5, 0.9);
    edit_blend(&mut project, 0.9, 0.5);
    write(tmp.path(), "characters/mia/face.toml", "profile = \"edited by hand\"\n");
    let err = project.save().unwrap_err();
    assert!(matches!(&err, StoreError::ChangedOnDisk { path } if path.ends_with("face.toml")), "{err}");
    assert_eq!(read(tmp.path(), "characters/mia/face.toml"), "profile = \"edited by hand\"\n");
    assert!(project.is_dirty());
}
