//! Files changed outside Omuzaki while the project is open (#382, journal.md 6): the save never overwrites one, saves
//! the rest and names every file it left alone; `project.reload` takes the outside changes in as one undoable step
//! (edited, created and deleted files, and a file replaced by a folder of the same name); undo puts the document back
//! and the next save writes it over the files; a clash with unsaved changes is reported, not merged.

// Test helpers may panic: a panic is a clear failure (clippy.toml exempts only `#[test]` functions).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::path::Path;
use std::time::Duration;

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc::Project;
use omuzaki_doc::journal::JournalOptions;
use omuzaki_doc_api::{DocPath, DocRead, Patch};
use omuzaki_history_api::JournalStatus;
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Sets (or, with no value, removes) the value at a path.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Put {
    path: String,
    value: Option<Value>,
}

impl Op for Put {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.put",
            summary: "Set the value at a path.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = DocPath::parse(&self.path).map_err(|e| OpError::invalid("path", e.to_string()))?;
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), self.value.clone())))
    }
}
register_op!(Put);

/// Saves only when asked, so a test decides when the files are written.
fn options() -> AppOptions {
    AppOptions::headless(Actor::Person { name: "pat".into() })
        .with_journal(JournalOptions::default().with_save_delays(Duration::from_secs(600), Duration::from_secs(600)))
}

/// A project with `notes.toml` (`x = 1`) and `other.toml` (`z = 0`) beside `project.toml`, open.
fn open_with_notes(dir: &Path) -> AppCore {
    AppCore::create(dir, "demo", options()).unwrap().close().unwrap();
    std::fs::write(dir.join("notes.toml"), "x = 1\n").unwrap();
    std::fs::write(dir.join("other.toml"), "z = 0\n").unwrap();
    AppCore::open(dir, options()).unwrap()
}

fn put(core: &AppCore, path: &str, value: &Value) {
    core.run("test.put", json!({ "path": path, "value": value })).unwrap();
}

fn get(core: &AppCore, path: &str) -> Option<Value> {
    core.get(&DocPath::parse(path).unwrap())
}

fn on_disk(dir: &Path, path: &str) -> Option<Value> {
    Project::open(dir).unwrap().get(&DocPath::parse(path).unwrap()).cloned()
}

fn save(core: &AppCore) -> Result<(), OpError> {
    core.run("project.save", json!({})).map(drop)
}

fn reload(core: &AppCore) -> Value {
    core.run("project.reload", json!({})).unwrap().plan.output.unwrap()
}

#[test]
fn an_edited_file_is_left_alone_named_and_the_rest_is_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let core = open_with_notes(dir);
    put(&core, "notes/x", &json!(2));
    put(&core, "other/z", &json!(4));
    std::fs::write(dir.join("notes.toml"), "x = 5 # by hand\n").unwrap();

    assert!(save(&core).is_err(), "the save refused a file");
    assert_eq!(std::fs::read_to_string(dir.join("notes.toml")).unwrap(), "x = 5 # by hand\n", "never overwritten");
    assert_eq!(on_disk(dir, "other/z"), Some(json!(4)), "the other files are saved");
    let JournalStatus::Failing { reason } = core.journal_status() else { panic!("{:?}", core.journal_status()) };
    assert!(reason.contains("notes.toml") && reason.contains("project.reload"), "{reason}");
    assert!(!reason.contains("other.toml"), "{reason}");

    // Reload takes theirs in, and says it replaced our unsaved change there.
    let output = reload(&core);
    assert_eq!(output["files"], json!(["notes.toml"]));
    assert_eq!(output["clashes"], json!(["notes.toml"]));
    assert_eq!(get(&core, "notes/x"), Some(json!(5)));
    save(&core).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("notes.toml")).unwrap(), "x = 5 # by hand\n", "their text is kept");
    assert_eq!(core.journal_status(), JournalStatus::Healthy);

    // Undo brings our change back, and the next save writes it over the file.
    core.undo().unwrap();
    assert_eq!(get(&core, "notes/x"), Some(json!(2)));
    save(&core).unwrap();
    assert_eq!(on_disk(dir, "notes/x"), Some(json!(2)));
    core.close().unwrap();
}

#[test]
fn a_created_file_comes_in_and_its_undo_deletes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let core = open_with_notes(dir);
    std::fs::write(dir.join("extra.toml"), "y = 3\n").unwrap();
    let output = reload(&core);
    assert_eq!((output["files"].clone(), output["clashes"].clone()), (json!(["extra.toml"]), json!([])));
    assert_eq!(get(&core, "extra/y"), Some(json!(3)));
    save(&core).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("extra.toml")).unwrap(), "y = 3\n");

    core.undo().unwrap();
    assert_eq!(get(&core, "extra"), None);
    save(&core).unwrap();
    assert!(!dir.join("extra.toml").exists(), "the next save follows the document");
    core.close().unwrap();
}

#[test]
fn a_deleted_file_leaves_the_document_and_its_undo_writes_it_again() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let core = open_with_notes(dir);
    std::fs::remove_file(dir.join("notes.toml")).unwrap();
    let output = reload(&core);
    assert_eq!(output["files"], json!(["notes.toml"]));
    assert_eq!(get(&core, "notes"), None);
    save(&core).unwrap();
    assert!(!dir.join("notes.toml").exists());

    core.undo().unwrap();
    save(&core).unwrap();
    assert_eq!(on_disk(dir, "notes/x"), Some(json!(1)));
    core.close().unwrap();
}

#[test]
fn a_file_replaced_by_a_folder_of_the_same_name_comes_in_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let core = open_with_notes(dir);
    std::fs::remove_file(dir.join("notes.toml")).unwrap();
    std::fs::create_dir(dir.join("notes")).unwrap();
    std::fs::write(dir.join("notes").join("a.toml"), "y = 1\n").unwrap();
    let output = reload(&core);
    assert_eq!(output["files"], json!(["notes.toml", "notes/a.toml"]));
    assert_eq!(get(&core, "notes"), Some(json!({"a": {"y": 1}})));
    save(&core).unwrap();
    core.close().unwrap();
    // The folder opens as it is: no `notes.toml` came back beside it.
    assert!(!dir.join("notes.toml").exists());
    assert_eq!(on_disk(dir, "notes/a/y"), Some(json!(1)));
}

#[test]
fn reload_with_nothing_changed_outside_changes_nothing_and_keeps_unsaved_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let core = open_with_notes(dir);
    put(&core, "notes/x", &json!(9));
    let output = reload(&core);
    assert_eq!((output["files"].clone(), output["reloaded"].clone()), (json!([]), json!([])));
    assert_eq!(get(&core, "notes/x"), Some(json!(9)), "an unsaved edit is not an outside change");
    core.close().unwrap();
}
