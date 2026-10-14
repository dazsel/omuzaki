//! The project store end to end: create, edit through patches, save, reopen.

// Helpers outside `#[test]` functions may unwrap too: a failed setup should just panic.
#![allow(clippy::unwrap_used)]

mod common;

use common::{change, exists, p, read, snapshot, write};
use omuzaki_doc::{Project, SaveReport, StoreError};
use omuzaki_doc_api::{DocRead, DocVersion, DocWrite, Patch, PatchError};
use serde_json::json;
use tempfile::TempDir;

fn new_project() -> (TempDir, Project) {
    let tmp = TempDir::new().unwrap();
    let project = Project::create(tmp.path().join("show.omuzaki"), "Show").unwrap();
    (tmp, project)
}

#[test]
fn create_writes_project_toml_and_open_reads_it_back() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("my-show.omuzaki"); // doesn't exist yet
    let project = Project::create(&dir, "My Show").unwrap();
    assert_eq!(project.root(), &json!({"project": {"name": "My Show", "schema": 1}}));
    assert_eq!(read(&dir, "project.toml"), "name = \"My Show\"\nschema = 1\n");
    assert!(!project.is_dirty());
    assert_eq!(project.version(), DocVersion(0));

    let reopened = Project::open(&dir).unwrap();
    assert_eq!(reopened.root(), project.root());
    assert_eq!(reopened.schema_version(), Some(1));
    assert_eq!(reopened.upgraded_from(), None);
    assert_eq!(reopened.dir(), dir);
}

#[test]
fn create_refuses_a_blank_name_and_an_existing_project() {
    let tmp = TempDir::new().unwrap();
    assert!(matches!(Project::create(tmp.path(), "  "), Err(StoreError::EmptyName)));
    Project::create(tmp.path(), "A").unwrap();
    assert!(matches!(Project::create(tmp.path(), "B"), Err(StoreError::AlreadyExists { .. })));
    assert_eq!(read(tmp.path(), "project.toml"), "name = \"A\"\nschema = 1\n");
}

#[test]
fn patches_save_and_reopen_to_an_equal_tree() {
    let (_tmp, mut project) = new_project();
    let dir = project.dir().to_path_buf();
    let patch = Patch::new()
        .with(
            p("characters"),
            None,
            Some(json!({"mira": {
                "character": {"parts": ["head", "body"]},
                "face": {"profile": "round", "eyes": {"size": 4}},
                "rig": {"template": "humanoid"},
                "variants": {},
            }})),
        )
        .with(
            p("scenes"),
            None,
            Some(json!({"opening": {"stage": "dark", "lights": [{"kind": "key"}, {"kind": "fill"}]}})),
        );
    assert_eq!(project.apply(&patch).unwrap(), DocVersion(1));
    assert!(project.is_dirty());

    assert_eq!(project.save().unwrap(), SaveReport { written: 5, removed: 0 });
    assert!(!project.is_dirty());
    for file in ["character", "face", "rig", "variants"] {
        assert!(exists(&dir, &format!("characters/mira/{file}.toml")), "{file}");
    }
    assert_eq!(read(&dir, "characters/mira/variants.toml"), "", "an empty object is an empty file");
    assert_eq!(
        read(&dir, "scenes/opening.toml"),
        "stage = \"dark\"\n\n[[lights]]\nkind = \"key\"\n\n[[lights]]\nkind = \"fill\"\n"
    );

    let reopened = Project::open(&dir).unwrap();
    assert_eq!(reopened.root(), project.root());
    assert!(!reopened.is_dirty());
}

#[test]
fn nothing_touches_the_disk_until_save() {
    let (_tmp, mut project) = new_project();
    let dir = project.dir().to_path_buf();
    let before = snapshot(&dir);
    project.apply(&change("characters", None, Some(json!({"a": {"face": {"x": 1}}})))).unwrap();
    project.apply(&change("project/name", Some(json!("Show")), Some(json!("Renamed")))).unwrap();
    assert_eq!(snapshot(&dir), before);
    drop(project);
    assert_eq!(Project::open(&dir).unwrap().root(), &json!({"project": {"name": "Show", "schema": 1}}));
}

#[test]
fn a_hand_edited_file_keeps_its_comments_and_untouched_files_are_not_rewritten() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let project_toml = "# my show\nname = \"Show\" # display name\nschema = 1\n";
    let face = "# round faces read friendly\nprofile = \"round\"  # keep\n\n# the eyes\n[eyes]\nsize = 4 # px\n";
    write(dir, "project.toml", project_toml);
    write(dir, "characters/mia/face.toml", face);
    write(dir, "characters/mia/rig.toml", "template   = \"humanoid\"\n");
    let mut project = Project::open(dir).unwrap();

    // An unrelated file changes: the commented ones are not even rewritten.
    project.apply(&change("characters/mia/rig/template", Some(json!("humanoid")), Some(json!("quad")))).unwrap();
    assert_eq!(project.save().unwrap(), SaveReport { written: 1, removed: 0 });
    assert_eq!(read(dir, "project.toml"), project_toml);
    assert_eq!(read(dir, "characters/mia/face.toml"), face);
    assert_eq!(read(dir, "characters/mia/rig.toml"), "template   = \"quad\"\n");

    // A change in the commented file keeps everything around it.
    project.apply(&change("characters/mia/face/eyes/size", Some(json!(4)), Some(json!(5)))).unwrap();
    project.apply(&change("characters/mia/face/blend", None, Some(json!(0.5)))).unwrap();
    project.save().unwrap();
    assert_eq!(
        read(dir, "characters/mia/face.toml"),
        "# round faces read friendly\nprofile = \"round\"  # keep\nblend = 0.5\n\n# the eyes\n[eyes]\nsize = 5 # px\n"
    );
    assert_eq!(read(dir, "project.toml"), project_toml);
}

#[test]
fn save_writes_only_the_files_that_changed() {
    let (_tmp, mut project) = new_project();
    let dir = project.dir().to_path_buf();
    project
        .apply(&change(
            "characters",
            None,
            Some(json!({"a": {"face": {"x": 1}, "rig": {"y": 1}}, "b": {"face": {"x": 1}}})),
        ))
        .unwrap();
    assert_eq!(project.save().unwrap().written, 3);
    let before = snapshot(&dir);

    assert_eq!(project.save().unwrap(), SaveReport::default(), "a second save has nothing to do");
    project.apply(&change("characters/a/rig/y", Some(json!(1)), Some(json!(2)))).unwrap();
    assert_eq!(project.save().unwrap(), SaveReport { written: 1, removed: 0 });
    let after = snapshot(&dir);
    let changed: Vec<_> = before.iter().zip(&after).filter(|(b, a)| b != a).map(|(b, _)| b.0.as_str()).collect();
    assert_eq!(changed, ["characters/a/rig.toml"]);

    // A change that is undone before saving leaves the file as it was.
    project.apply(&change("characters/b/face/x", Some(json!(1)), Some(json!(9)))).unwrap();
    project.apply(&change("characters/b/face/x", Some(json!(9)), Some(json!(1)))).unwrap();
    assert_eq!(project.save().unwrap().written, 0);
    assert_eq!(snapshot(&dir), after);
}

#[test]
fn conflicts_and_unreachable_paths_leave_everything_untouched() {
    let (_tmp, mut project) = new_project();
    project.apply(&change("characters", None, Some(json!({"a": {"face": {"x": 1}}})))).unwrap();
    project.save().unwrap();
    let root = project.root().clone();
    let version = project.version();
    let files = snapshot(project.dir());

    let conflict = Patch::new()
        .with(p("characters/a/face/x"), Some(json!(1)), Some(json!(2)))
        .with(p("characters/a/face/y"), None, Some(json!(3)))
        .with(p("characters/a/face/x"), Some(json!(999)), Some(json!(4)));
    let err = project.apply(&conflict).unwrap_err();
    assert!(matches!(err, PatchError::Conflict { ref path, .. } if path.to_string() == "characters/a/face/x"));

    let orphan = Patch::new().with(p("characters/b"), None, Some(json!({"face": {"x": 1}}))).with(
        p("characters/zzz/face"),
        None,
        Some(json!({"x": 1})),
    );
    assert!(matches!(project.apply(&orphan).unwrap_err(), PatchError::Unreachable { .. }));

    assert_eq!(project.root(), &root);
    assert_eq!(project.version(), version);
    assert!(!project.is_dirty(), "a refused patch must not leave pending saves behind");
    assert_eq!(project.save().unwrap(), SaveReport::default());
    assert_eq!(snapshot(project.dir()), files);
}

#[test]
fn what_toml_cannot_hold_is_refused_with_a_clear_error() {
    let (_tmp, mut project) = new_project();
    let root = project.root().clone();
    let cases = [
        (change("settings", None, Some(json!({"a": null}))), "null"),
        (change("settings", None, Some(json!({"a": {"b": [1, null]}}))), "settings/a/b/1"),
        (change("settings", None, Some(json!({"big": u64::MAX}))), "9223372036854775807"),
        (change("flag", None, Some(json!(true))), "must be objects"),
        (change("", Some(root.clone()), Some(json!({}))), "root"),
    ];
    for (patch, needle) in cases {
        let PatchError::Unreachable { reason, .. } = project.apply(&patch).unwrap_err() else { panic!("wrong error") };
        assert!(reason.contains(needle), "{reason:?} should mention {needle:?}");
    }
    assert_eq!(project.root(), &root);
    assert_eq!(project.version(), DocVersion(0));
}

#[test]
fn names_that_cannot_be_files_are_refused_where_they_would_matter() {
    let (_tmp, mut project) = new_project();
    project.apply(&change("characters", None, Some(json!({"a": {"face": {"x": 1}}})))).unwrap();
    project.save().unwrap();

    // A new entity in a folder becomes a file or folder name.
    let err = project.apply(&change("characters/Big Boss", None, Some(json!({"face": {"x": 1}})))).unwrap_err();
    let PatchError::Unreachable { path, reason } = err else { panic!("wrong error") };
    assert_eq!(path.to_string(), "characters/Big Boss");
    assert!(reason.contains("slug"), "{reason}");
    assert!(matches!(project.apply(&change("../escape", None, Some(json!({})))), Err(PatchError::Unreachable { .. })));
    assert!(!project.is_dirty());

    // Inside a file any key is fine, and a new node whose keys can't be names simply becomes a file.
    project.apply(&change("characters/a/face/Left Eye", None, Some(json!({"size": 3})))).unwrap();
    project.apply(&change("characters/b", None, Some(json!({"Left Eye": {"size": 3}})))).unwrap();
    project.save().unwrap();
    let dir = project.dir();
    assert!(exists(dir, "characters/b.toml") && !exists(dir, "characters/b"));
    assert_eq!(Project::open(dir).unwrap().root(), project.root());
}

#[test]
fn files_and_folders_follow_the_content() {
    let (_tmp, mut project) = new_project();
    let dir = project.dir().to_path_buf();
    project
        .apply(&change("characters", None, Some(json!({"a": {"face": {"x": 1}, "variants": {"game": {"scale": 2}}}}))))
        .unwrap();
    project.save().unwrap();
    assert!(exists(&dir, "characters/a/variants/game.toml"));

    // Removing the last variant leaves an empty object, which is an empty file: a folder would not come back.
    project.apply(&change("characters/a/variants/game", Some(json!({"scale": 2})), None)).unwrap();
    project.save().unwrap();
    assert!(!exists(&dir, "characters/a/variants"), "the emptied folder is pruned");
    assert_eq!(read(&dir, "characters/a/variants.toml"), "");
    assert_eq!(Project::open(&dir).unwrap().root(), project.root());

    // Adding one again turns it back into a folder of files, as the spec lays it out.
    project.apply(&change("characters/a/variants/vr", None, Some(json!({"scale": 3})))).unwrap();
    project.save().unwrap();
    assert!(!exists(&dir, "characters/a/variants.toml"));
    assert_eq!(read(&dir, "characters/a/variants/vr.toml"), "scale = 3\n");

    // A plain value next to the files would merge them into one and lose their comments: refused, with a way out.
    let err = project.apply(&change("characters/a/note", None, Some(json!("hi")))).unwrap_err();
    let PatchError::Unreachable { path, reason } = err else { panic!("wrong error") };
    assert_eq!(path.to_string(), "characters/a/note");
    assert!(reason.contains("face, variants") && reason.contains("inside one of those files"), "{reason}");
    assert!(!project.is_dirty());
    project.apply(&change("characters/a/face/note", None, Some(json!("hi")))).unwrap();
    project.save().unwrap();
    assert!(read(&dir, "characters/a/face.toml").contains("note = \"hi\""));
    assert_eq!(Project::open(&dir).unwrap().root(), project.root());

    // Removing an entity deletes its files and the folders they leave empty.
    project.apply(&change("characters/a", project.get(&p("characters/a")).cloned(), None)).unwrap();
    project.save().unwrap();
    assert!(!exists(&dir, "characters/a") && !exists(&dir, "characters/a.toml"));
    assert_eq!(read(&dir, "characters.toml"), "", "characters is now an empty object, so an empty file");
    assert_eq!(Project::open(&dir).unwrap().root(), project.root());
}

#[test]
fn a_plain_value_next_to_hand_written_files_is_refused_and_nothing_changes() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let face = "# my face notes\nborn = 2026-10-11 # keep\nprofile = \"round\" # keep\n";
    let rig = "# rig comment\nbones = 3\n";
    write(dir, "project.toml", "name = \"x\"\nschema = 1\n");
    write(dir, "characters/mia/face.toml", face);
    write(dir, "characters/mia/rig.toml", rig);
    let mut project = Project::open(dir).unwrap();
    let root = project.root().clone();

    let err = project.apply(&change("characters/mia/nickname", None, Some(json!("D")))).unwrap_err();
    assert!(matches!(err, PatchError::Unreachable { .. }), "{err}");
    assert_eq!(project.root(), &root);
    assert_eq!(project.save().unwrap(), SaveReport::default());
    assert_eq!(read(dir, "characters/mia/face.toml"), face, "comments and the date are untouched");
    assert_eq!(read(dir, "characters/mia/rig.toml"), rig);

    // Putting it inside a file keeps every comment and the date as a date.
    project.apply(&change("characters/mia/rig/nickname", None, Some(json!("D")))).unwrap();
    project.save().unwrap();
    assert_eq!(read(dir, "characters/mia/rig.toml"), "# rig comment\nbones = 3\nnickname = \"D\"\n");
    assert_eq!(read(dir, "characters/mia/face.toml"), face);
}

#[test]
fn files_the_store_does_not_own_survive_every_save() {
    let (_tmp, mut project) = new_project();
    let dir = project.dir().to_path_buf();
    write(&dir, "library.lock", "lock");
    write(&dir, ".omuzaki/cache.toml", "a = 1\n");
    project.apply(&change("performances", None, Some(json!({"p1": {"score": {"keys": [1, 2]}}})))).unwrap();
    project.save().unwrap();
    write(&dir, "performances/p1/takes/1.track", "binary");

    project.apply(&change("performances/p1", project.get(&p("performances/p1")).cloned(), None)).unwrap();
    project.save().unwrap();
    assert_eq!(read(&dir, "library.lock"), "lock");
    assert_eq!(read(&dir, ".omuzaki/cache.toml"), "a = 1\n");
    assert_eq!(read(&dir, "performances/p1/takes/1.track"), "binary", "media is never deleted");
    let reopened = Project::open(&dir).unwrap();
    assert!(reopened.get(&p("performances/p1/takes")).is_none(), "folders without TOML are not in the tree");
}

#[test]
fn an_inverse_patch_restores_the_files() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "project.toml", "name = \"S\"\nschema = 1\n");
    write(dir, "characters/a/face.toml", "# note\nx = 1\n");
    let original = snapshot(dir);
    let mut project = Project::open(dir).unwrap();

    let patch = Patch::new()
        .with(p("characters/a/face/x"), Some(json!(1)), Some(json!(2)))
        .with(p("characters/a/rig"), None, Some(json!({"y": 1})))
        .with(p("scenes"), None, Some(json!({"s": {"stage": "x"}})));
    project.apply(&patch).unwrap();
    project.save().unwrap();
    assert_ne!(snapshot(dir), original);

    project.apply(&patch.inverse()).unwrap();
    project.save().unwrap();
    assert_eq!(snapshot(dir), original);
}

#[test]
fn array_items_and_nested_values_save_in_place() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "project.toml", "name = \"S\"\nschema = 1\n");
    write(dir, "scenes/s.toml", "xs = [1, 2]\n\n[[props]]\nname = \"a\"\n");
    let mut project = Project::open(dir).unwrap();
    project.apply(&change("scenes/s/xs/2", None, Some(json!(3)))).unwrap();
    project.apply(&change("scenes/s/xs/0", Some(json!(1)), Some(json!(10)))).unwrap();
    project.apply(&change("scenes/s/props/1", None, Some(json!({"name": "b"})))).unwrap();
    project.save().unwrap();
    assert_eq!(read(dir, "scenes/s.toml"), "xs = [10, 2, 3]\n\n[[props]]\nname = \"a\"\n\n[[props]]\nname = \"b\"\n");
    assert_eq!(Project::open(dir).unwrap().root(), project.root());
}

#[test]
fn opening_reports_what_is_wrong_with_the_folder() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    assert!(matches!(Project::open(dir), Err(StoreError::NotAProject { .. })));

    write(dir, "project.toml", "name = \"S\"\n");
    assert!(matches!(Project::open(dir), Err(StoreError::BadSchema(_))));
    write(dir, "project.toml", "name = \"S\"\nschema = 2\n");
    assert!(matches!(Project::open(dir), Err(StoreError::SchemaTooNew { found: 2, supported: 1 })));
    let message = Project::open(dir).unwrap_err().to_string();
    assert!(message.contains("schema 2") && message.contains("update Omuzaki"), "{message}");

    write(dir, "project.toml", "schema = 1\n");
    write(dir, "scenes/broken.toml", "a = [1,\n");
    let err = Project::open(dir).unwrap_err();
    assert!(matches!(err, StoreError::Syntax { .. }));
    assert!(err.to_string().contains("broken.toml"), "{err}");

    write(dir, "scenes/broken.toml", "x = nan\n");
    assert!(matches!(Project::open(dir), Err(StoreError::Unsupported { .. })));
}

#[test]
fn a_file_broken_by_hand_is_never_overwritten() {
    let (_tmp, mut project) = new_project();
    let dir = project.dir().to_path_buf();
    project.apply(&change("scenes", None, Some(json!({"s": {"stage": "x"}})))).unwrap();
    project.save().unwrap();

    write(&dir, "scenes/s.toml", "stage = \n");
    project.apply(&change("scenes/s/stage", Some(json!("x")), Some(json!("y")))).unwrap();
    assert!(matches!(project.save(), Err(StoreError::Syntax { .. })));
    assert_eq!(read(&dir, "scenes/s.toml"), "stage = \n");
    assert!(project.is_dirty(), "the change is still waiting to be saved");

    write(&dir, "scenes/s.toml", "stage = \"x\"\n");
    assert_eq!(project.save().unwrap().written, 1);
    assert_eq!(read(&dir, "scenes/s.toml"), "stage = \"y\"\n");
}

#[test]
fn a_file_deleted_behind_the_stores_back_is_written_again() {
    let (_tmp, mut project) = new_project();
    let dir = project.dir().to_path_buf();
    project.apply(&change("scenes", None, Some(json!({"s": {"stage": "x"}})))).unwrap();
    project.save().unwrap();
    std::fs::remove_file(dir.join("scenes/s.toml")).unwrap();
    project.apply(&change("scenes/s/stage", Some(json!("x")), Some(json!("y")))).unwrap();
    assert_eq!(project.save().unwrap().written, 1);
    assert_eq!(read(&dir, "scenes/s.toml"), "stage = \"y\"\n");
}

#[test]
fn editing_never_reorganises_a_hand_made_layout() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "project.toml", "name = \"S\"\nschema = 1\n");
    // Only tables, so the store could have made this a folder of files; it was a file, so it stays one.
    write(dir, "scenes/s1.toml", "[stage]\nw = 1\n\n[cam]\nfov = 2\n");
    let mut project = Project::open(dir).unwrap();
    project.apply(&change("scenes/s1/cam/fov", Some(json!(2)), Some(json!(3)))).unwrap();
    project.apply(&change("scenes/s1/stage", Some(json!({"w": 1})), None)).unwrap();
    project.save().unwrap();
    assert_eq!(read(dir, "scenes/s1.toml"), "\n[cam]\nfov = 3\n");
    assert!(!exists(dir, "scenes/s1"));
    assert_eq!(Project::open(dir).unwrap().root(), project.root());
}

#[test]
fn leftovers_of_an_interrupted_save_are_ignored_and_replaced() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(dir, "project.toml", "name = \"S\"\nschema = 1\n");
    write(dir, "scenes/s.toml", "stage = \"x\"\n");
    write(dir, "scenes/.s.toml.tmp", "half a fi");
    let mut project = Project::open(dir).unwrap();
    assert_eq!(project.get(&p("scenes")), Some(&json!({"s": {"stage": "x"}})));
    project.apply(&change("scenes/s/stage", Some(json!("x")), Some(json!("y")))).unwrap();
    project.save().unwrap();
    assert_eq!(read(dir, "scenes/s.toml"), "stage = \"y\"\n");
    assert!(!exists(dir, "scenes/.s.toml.tmp"));
}

fn takes_a_doc(doc: &dyn DocRead) -> Option<String> {
    doc.get(&p("project/name")).and_then(|v| v.as_str()).map(str::to_owned)
}

#[test]
fn the_tree_is_read_through_the_doc_api() {
    let (_tmp, project) = new_project();
    assert_eq!(project.get(&p("project/name")), Some(&json!("Show")));
    assert_eq!(project.get(&p("project/missing")), None);
    assert_eq!(takes_a_doc(&project).as_deref(), Some("Show"));
}

#[test]
fn a_two_hundred_file_project_opens_quickly() {
    let tmp = TempDir::new().unwrap();
    common::write_big_project(tmp.path(), 200);
    let mut fastest = std::time::Duration::MAX;
    for _ in 0..5 {
        let start = std::time::Instant::now();
        let project = Project::open(tmp.path()).unwrap();
        fastest = fastest.min(start.elapsed());
        assert_eq!(project.get(&p("characters")).and_then(|c| c.as_object()).map(serde_json::Map::len), Some(34));
    }
    eprintln!("opening 200 files: {fastest:?} (fastest of 5)");
    // The real 50 ms budget is checked by the `open` bench on a quiet machine (budget harness); this guard
    // only catches gross regressions, because CI shares a loaded machine.
    assert!(fastest < std::time::Duration::from_millis(500), "opening took {fastest:?}");
}
