//! `Project::write_exact` (journal.md 8.5): recovery and restore write the folder to match a project exactly, which
//! clears a layout-changing save a crash cut short, and an interrupted run completes when run again.

// Helpers outside `#[test]` may unwrap.
#![allow(clippy::unwrap_used)]

mod common;

use common::{read, snapshot_except, write};
use omuzaki_doc::Project;
use omuzaki_doc_api::DocRead;
use serde_json::json;

/// The project recovery wants: Mia's face as a folder of parts.
fn wanted() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "project.toml", "name = \"Show\"\nschema = 1\n");
    write(tmp.path(), "characters/mia/face/eyes.toml", "size = 0.6\n");
    write(tmp.path(), "characters/mia/face/mouth.toml", "kind = \"cat\"\n");
    tmp
}

fn files(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    snapshot_except(dir, &[".omuzaki"])
}

#[test]
fn a_file_and_a_folder_of_one_name_left_by_a_crash_are_cleared_and_the_newer_data_kept() {
    let want = wanted();
    let target = tempfile::tempdir().unwrap();
    // The crash came between writing the folder's files and deleting the old file of the same name.
    write(target.path(), "project.toml", "name = \"Old\"\nschema = 1\n");
    write(target.path(), "characters/mia/face.toml", "size = 0.4\n");
    write(target.path(), "characters/mia/face/eyes.toml", "size = 0.6\n");
    write(target.path(), "takes/one.track", "beside the tree");
    write(target.path(), ".git/HEAD", "ref");
    assert!(Project::open(target.path()).is_err(), "ambiguous as the crash left it");

    let mut project = Project::open_files(target.path(), files(want.path())).unwrap();
    let report = project.write_exact().unwrap();
    assert_eq!((report.written, report.removed), (2, 1), "{report:?}");
    let reopened = Project::open(target.path()).unwrap();
    assert_eq!(reopened.get(&common::p("characters/mia/face/eyes/size")), Some(&json!(0.6)));
    assert_eq!(reopened.get(&common::p("project/name")), Some(&json!("Show")));
    assert_eq!(read(target.path(), "takes/one.track"), "beside the tree", "files beside the tree are untouched");
    assert_eq!(read(target.path(), ".git/HEAD"), "ref", "hidden names are untouched");
}

#[test]
fn an_interrupted_write_completes_when_run_again_and_a_second_run_changes_nothing() {
    let want = wanted();
    // Every state a run can stop in: nothing done, the parts written, the stray removed, all but project.toml.
    let states: [&[(&str, &str)]; 3] = [
        &[("project.toml", "name = \"Old\"\nschema = 1\n"), ("characters/mia/face.toml", "size = 0.4\n")],
        &[
            ("project.toml", "name = \"Old\"\nschema = 1\n"),
            ("characters/mia/face.toml", "size = 0.4\n"),
            ("characters/mia/face/eyes.toml", "size = 0.6\n"),
            ("characters/mia/face/mouth.toml", "kind = \"cat\"\n"),
        ],
        &[
            ("project.toml", "name = \"Old\"\nschema = 1\n"),
            ("characters/mia/face/eyes.toml", "size = 0.6\n"),
            ("characters/mia/face/mouth.toml", "kind = \"cat\"\n"),
        ],
    ];
    for (i, state) in states.iter().enumerate() {
        let target = tempfile::tempdir().unwrap();
        for (rel, text) in *state {
            write(target.path(), rel, text);
        }
        let mut project = Project::open_files(target.path(), files(want.path())).unwrap();
        project.write_exact().unwrap();
        assert_eq!(files(target.path()), files(want.path()), "state {i}");
        let again = project.write_exact().unwrap();
        assert_eq!((again.written, again.removed), (0, 0), "state {i}: idempotent");
    }
}

#[test]
fn after_an_exact_write_ordinary_saves_work_from_the_files() {
    let want = wanted();
    let target = tempfile::tempdir().unwrap();
    write(target.path(), "project.toml", "name = \"Old\"\nschema = 1\n");
    let mut project = Project::open_files(target.path(), files(want.path())).unwrap();
    project.write_exact().unwrap();
    let before = project.get(&common::p("project/name")).cloned();
    omuzaki_doc_api::DocWrite::apply(
        &mut project,
        &omuzaki_doc_api::Patch::new().with(common::p("project/name"), before, Some(json!("Renamed"))),
    )
    .unwrap();
    project.save().unwrap();
    assert!(read(target.path(), "project.toml").contains("Renamed"));
}
