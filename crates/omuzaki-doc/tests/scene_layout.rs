//! The scene layout of ADR 0024 D1 on the real store: a `SceneDef` saves as exactly `scene.toml`, `lighting.toml`,
//! `cameras/<id>.toml` and `cast/<member>.toml`, and reopening gives the same `SceneDef`.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::{Path, PathBuf};

use common::{p, read, snapshot, write};
use omuzaki_doc::Project;
use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use omuzaki_stage_api::{SceneDef, SceneId, paths};
use serde_json::Value;
use tempfile::TempDir;

/// The fixture scene of `omuzaki-stage-api`'s tests: every TOML example of the scene design.
fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../omuzaki-stage-api/tests/fixtures/scene")
}

const FIXTURE_FILES: [&str; 7] = [
    "scene.toml",
    "lighting.toml",
    "cameras/full.toml",
    "cameras/face.toml",
    "cameras/orbit.toml",
    "cast/main.toml",
    "cast/kiri.toml",
];

/// A project that already has the fixture files under `scenes/main/`, as another tool would have written them.
fn project_with_fixture_files() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("show.omuzaki");
    Project::create(&dir, "Show").unwrap();
    for rel in FIXTURE_FILES {
        write(&dir, &format!("scenes/main/{rel}"), &read(&fixture_dir(), rel));
    }
    (tmp, dir)
}

#[test]
fn a_scene_saves_as_the_files_of_the_design_and_reopens_equal() {
    let (_keep, source_dir) = project_with_fixture_files();
    let source = Project::open(&source_dir).unwrap();
    let id = SceneId::new("main").unwrap();
    let def = SceneDef::from_node(source.get(&paths::scene(&id)).unwrap()).unwrap();

    // Save the typed scene through a fresh project: one patch that adds the node (its parent `scenes` is created
    // with it, as a first scene does).
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("fresh.omuzaki");
    let mut fresh = Project::create(&dir, "Fresh").unwrap();
    let mut node = Value::Null;
    def.to_node(&mut node).unwrap();
    fresh.apply(&Patch::new().with(p("scenes"), None, Some(serde_json::json!({"main": node})))).unwrap();
    fresh.save().unwrap();

    let files: Vec<String> = snapshot(&dir).into_iter().map(|(rel, _)| rel).collect();
    let mut expected: Vec<String> = FIXTURE_FILES.iter().map(|f| format!("scenes/main/{f}")).collect();
    expected.push("project.toml".into());
    expected.sort();
    assert_eq!(files, expected);

    assert!(read(&dir, "scenes/main/lighting.toml").contains("exposure = 0.9\n"), "floats are saved as typed");
    let reopened = Project::open(&dir).unwrap();
    let again = SceneDef::from_node(reopened.get(&paths::scene(&id)).unwrap()).unwrap();
    assert_eq!(again, def);
    assert_eq!(again.check().unwrap(), vec![]);
}

#[test]
fn the_default_scene_is_a_value_of_the_project_file() {
    let (_keep, dir) = project_with_fixture_files();
    let mut project = Project::open(&dir).unwrap();
    project.apply(&Patch::new().with(paths::default_scene(), None, Some(Value::from("main")))).unwrap();
    project.save().unwrap();
    assert!(read(&dir, "project.toml").contains("default_scene = \"main\""));
}
