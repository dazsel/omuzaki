//! On Windows a save replaces a file atomically: the original is kept by hard link in `.omuzaki/replaced`, then the
//! new text is renamed over the target with POSIX semantics. Runs in the Windows CI job; empty elsewhere.

#![cfg(windows)]
// Helpers outside `#[test]` functions may unwrap too: a failed setup should just panic.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;

use common::{change, read, write};
use omuzaki_doc::Project;
use omuzaki_doc_api::DocWrite;
use serde_json::json;
use tempfile::TempDir;

const FACE: &str = "profile = \"round\"\nblend = 0.5\n";

#[test]
fn a_save_replaces_an_existing_file_and_keeps_the_original() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "project.toml", "name = \"x\"\nschema = 1\n");
    write(tmp.path(), "characters/mia/face.toml", FACE);
    let mut project = Project::open(tmp.path()).unwrap();
    project.apply(&change("characters/mia/face/blend", Some(json!(0.5)), Some(json!(0.9)))).unwrap();
    assert_eq!(project.save().unwrap().written, 1);

    assert!(read(tmp.path(), "characters/mia/face.toml").contains("0.9"));
    let kept: Vec<String> = fs::read_dir(tmp.path().join(".omuzaki/replaced/characters/mia"))
        .unwrap()
        .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert_eq!(kept, [FACE]);
    // No temporary file is left beside the target.
    let names: Vec<_> =
        fs::read_dir(tmp.path().join("characters/mia")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["face.toml"]);
}
