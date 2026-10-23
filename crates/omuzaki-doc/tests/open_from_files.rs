//! `Project::open_files` reads a project from file bytes (a snapshot) by exactly the rules `Project::open` reads a
//! folder: the same tree, the same saves, the same errors.

// Helpers outside `#[test]` may unwrap.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;

use common::{snapshot, snapshot_except, write};
use omuzaki_doc::{Project, StoreError};
use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use serde_json::json;

/// The folders the tests compare: nested parts, comments, hidden and non-TOML files, empty files and folders.
fn fixtures() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("minimal", vec![("project.toml", "name = \"Show\"\nschema = 1\n")]),
        (
            "nested",
            vec![
                ("project.toml", "# my show\nname = \"Show\" # display name\nschema = 1\n"),
                ("characters/mia/character.toml", "name = \"Mia\"\n"),
                ("characters/mia/face.toml", "# keep this comment\nprofile = \"round\"\n[eyes]\nsize = 0.5\n"),
                ("characters/mia/parts/hair.toml", "kind = \"bob\"\nlocks = [1, 2, 3]\n"),
                ("characters/kai/character.toml", "name = \"Kai\"\n"),
                ("characters/kai/empty.toml", ""),
                ("scenes/desk.toml", "[light]\nkey = 1.25\n"),
            ],
        ),
        (
            "with others",
            vec![
                ("project.toml", "name = \"Show\"\nschema = 1\n"),
                ("assets.lock", "# not part of the tree\n"),
                ("takes/one.track", "binary-ish"),
                ("notes.md", "# notes\n"),
                (".hidden.toml", "secret = true\n"),
                (".omuzaki/id", "abc"),
                ("characters/.cache/x.toml", "a = 1\n"),
                ("characters/mia/character.toml", "name = \"Mia\"\n"),
                ("characters/mia/.toml", "nameless = true\n"),
            ],
        ),
    ]
}

fn make(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        write(tmp.path(), rel, text);
    }
    tmp
}

fn from_files(dir: &Path) -> Result<Project, StoreError> {
    Project::open_files(dir, snapshot(dir))
}

#[test]
fn a_folder_and_its_files_open_to_the_same_tree_and_save_the_same_files() {
    for (name, files) in fixtures() {
        let a = make(&files);
        let b = make(&files);
        let mut folder = Project::open(a.path()).unwrap();
        let mut bytes = from_files(b.path()).unwrap();
        assert_eq!(folder.root(), bytes.root(), "{name}");
        assert_eq!(folder.rendered_files().unwrap(), bytes.rendered_files().unwrap(), "{name}");
        assert_eq!(folder.file_hashes(), bytes.file_hashes(), "{name}");

        // The same edit saves the same bytes: the known texts (comments, layout) came across.
        let edit = |p: &mut Project| {
            let before = p.get(&common::p("project/name")).cloned();
            p.apply(&Patch::new().with(common::p("project/name"), before, Some(json!("Renamed")))).unwrap();
            p.apply(&Patch::new().with(common::p("extra"), None, Some(json!({"made": true})))).unwrap();
            p.save().unwrap();
        };
        edit(&mut folder);
        edit(&mut bytes);
        // `.omuzaki/replaced/` names its entries by time, so only the project's own files are compared.
        assert_eq!(snapshot_except(a.path(), &[".omuzaki"]), snapshot_except(b.path(), &[".omuzaki"]), "{name}");
    }
}

#[test]
fn opening_from_bytes_reads_and_writes_nothing_in_the_folder() {
    let empty = tempfile::tempdir().unwrap();
    let files = fixtures().remove(1).1;
    let source = make(&files);
    let project = Project::open_files(empty.path(), snapshot(source.path())).unwrap();
    assert_eq!(project.root()["characters"]["mia"]["face"]["eyes"]["size"], json!(0.5));
    assert_eq!(project.dir(), empty.path());
    assert!(snapshot(empty.path()).is_empty(), "the folder it would save into is untouched");
}

#[test]
fn the_error_cases_are_the_same_errors() {
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("not a project", vec![("characters/mia.toml", "a = 1\n")]),
        ("syntax", vec![("project.toml", "name = \"Show\"\n"), ("characters/mia.toml", "a = = 1\n")]),
        ("ambiguous", vec![("project.toml", "name = \"S\"\n"), ("mia.toml", "a = 1\n"), ("mia/face.toml", "b = 1\n")]),
        ("too new", vec![("project.toml", "name = \"S\"\nschema = 99999\n"), ("broken.toml", "= nope")]),
        ("not utf-8 is a file the folder can't read either", vec![("project.toml", "name = \"S\"\n")]),
    ];
    for (name, files) in cases {
        let a = make(&files);
        let mut bytes = snapshot(a.path());
        if name.starts_with("not utf-8") {
            std::fs::write(a.path().join("bad.toml"), [0xff, 0xfe]).unwrap();
            bytes.push(("bad.toml".into(), vec![0xff, 0xfe]));
        }
        let folder = Project::open(a.path()).unwrap_err();
        let from_bytes = Project::open_files(a.path(), bytes).unwrap_err();
        assert_eq!(format!("{folder}"), format!("{from_bytes}"), "{name}");
        assert_eq!(std::mem::discriminant(&folder), std::mem::discriminant(&from_bytes), "{name}");
    }
}
