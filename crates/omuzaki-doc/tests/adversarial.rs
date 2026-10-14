//! Hostile and awkward input: names that would escape the project or can't exist on some OS, and a project from
//! the future.
// Helpers outside `#[test]` functions may unwrap too: a failed setup should just panic.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::Path;

use common::{change, p, snapshot, write};
use omuzaki_doc::{Project, StoreError};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use serde_json::{Value, json};
use tempfile::TempDir;

const NASTY: &[&str] = &[
    "..",
    ".",
    "../x",
    "../../evil",
    "a/b",
    "a\\b",
    "/etc/x",
    "C:",
    "C:\\x",
    "con",
    "aux",
    "nul",
    "prn",
    "com1",
    "lpt1",
    "con.toml",
    "é",
    "e\u{301}",
    "a\0b",
    "A",
    " a",
    "a ",
    "a.",
    ".omuzaki",
    ".hidden",
    "\u{202e}gnp",
    "x.toml",
];

/// Every path under `dir`, relative to it, sorted.
fn all_entries(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            out.push(path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
            if path.is_dir() && !path.is_symlink() {
                walk(&path, base, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Everything under `outer` except `outer/inner/proj`.
fn outside(outer: &Path) -> Vec<String> {
    all_entries(outer).into_iter().filter(|e| !e.starts_with("inner/proj")).collect()
}

#[test]
fn odd_keys_never_write_outside_the_project_and_round_trip() {
    let tmp = TempDir::new().unwrap();
    let outer = tmp.path();
    write(outer, "sentinel.txt", "keep");
    write(outer, "inner/sibling.toml", "a = 1\n");
    let dir = outer.join("inner/proj");
    let mut project = Project::create(&dir, "P").unwrap();
    project.apply(&change("characters", None, Some(json!({"mia": {"face": {"a": 1}}})))).unwrap();
    project.save().unwrap();
    let before_outside = outside(outer);
    let sentinel: Vec<_> = snapshot(outer).into_iter().filter(|(n, _)| !n.starts_with("inner/proj")).collect();

    for (i, key) in NASTY.iter().enumerate() {
        let entity = json!({"face": {"a": 1}, "rig": {"b": 2}});
        let shapes: Vec<(DocPath, Value)> = vec![
            (DocPath::root().join(*key), entity.clone()),
            (p("characters").join(*key), entity),
            (p("characters/mia").join(*key), json!({"x": 1})),
            (p("characters/mia").join(*key), json!({"deep": {"x": 1}})),
            (p("scenes").join(format!("s{i}")), json!({*key: {"a": {"b": 1}}})),
        ];
        for (path, value) in shapes {
            if path.segments()[0] == "scenes" && project.get(&p("scenes")).is_none() {
                project.apply(&change("scenes", None, Some(json!({"s_init": {"k": 1}})))).unwrap();
            }
            let before = project.get(&path).cloned();
            // Whether the patch is accepted doesn't matter here; what matters is what ends up on disk.
            drop(project.apply(&Patch::new().with(path.clone(), before, Some(value))));
            let saved = project.save();
            assert!(saved.is_ok(), "save failed for {key:?} at {path}: {saved:?}");
            match Project::open(&dir) {
                Ok(reopened) => assert_eq!(reopened.root(), project.root(), "round trip differs for {key:?} at {path}"),
                Err(e) => panic!("project no longer opens after {key:?} at {path}: {e}"),
            }
        }
    }
    assert_eq!(outside(outer), before_outside, "something appeared or vanished outside the project");
    let after: Vec<_> = snapshot(outer).into_iter().filter(|(n, _)| !n.starts_with("inner/proj")).collect();
    assert_eq!(after, sentinel, "a file outside the project changed");
}

#[test]
fn windows_reserved_names_are_never_created_as_files_or_folders() {
    let reserved = ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9"];
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("p");
    let mut project = Project::create(&dir, "P").unwrap();
    for name in reserved {
        let as_entity = project.apply(&change(name, None, Some(json!({"face": {"a": 1}, "rig": {"b": 2}}))));
        assert!(as_entity.is_err(), "{name} was accepted as a top-level folder");
        // As a key it is fine where it stays inside a file.
        project.apply(&change(&format!("chars_{name}"), None, Some(json!({name: {"a": {"b": 1}}})))).unwrap();
    }
    project.save().unwrap();
    let bad: Vec<String> = all_entries(&dir)
        .into_iter()
        .filter(|e| {
            e.split('/')
                .any(|seg| reserved.contains(&seg.split('.').next().unwrap_or("").to_ascii_lowercase().as_str()))
        })
        .collect();
    assert!(bad.is_empty(), "names that Windows treats as devices were created: {bad:?}");
    assert_eq!(Project::open(&dir).unwrap().root(), project.root());
}

#[test]
fn a_newer_schema_is_reported_even_when_other_files_use_unknown_things() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "project.toml", "name = \"x\"\nschema = 99\n");
    write(tmp.path(), "future.toml", "v = inf\n");
    write(tmp.path(), "broken.toml", "a = \n");
    let err = Project::open(tmp.path()).unwrap_err();
    assert!(matches!(err, StoreError::SchemaTooNew { found: 99, .. }), "{err}");
}
