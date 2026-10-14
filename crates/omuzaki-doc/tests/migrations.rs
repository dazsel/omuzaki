//! Migrations: registered like any other (the registry is per test binary, so this file's two are the only ones).

// The test migrations must have the fallible signature of the real ones; helpers outside `#[test]` may unwrap.
#![allow(clippy::unnecessary_wraps, clippy::unwrap_used)]

mod common;

use common::{read, snapshot, write};
use omuzaki_doc::{MigrationError, Project, SaveReport, StoreError, register_migration, supported_schema};
use omuzaki_doc_api::DocRead;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Schema 2: a character's `look.outline` is renamed `look.outline_width`.
fn rename_outline(project: &mut Value) -> Result<(), MigrationError> {
    let Some(characters) = project.get_mut("characters").and_then(Value::as_object_mut) else { return Ok(()) };
    for character in characters.values_mut() {
        let Some(look) = character.get_mut("look").and_then(Value::as_object_mut) else { continue };
        if let Some(outline) = look.remove("outline") {
            look.insert("outline_width".to_owned(), outline);
        }
    }
    Ok(())
}
register_migration!(2, "rename_outline", rename_outline);

/// Schema 3: adds a `settings` file; refuses a project that asks to be refused.
fn add_settings(project: &mut Value) -> Result<(), MigrationError> {
    if project["project"].get("poison").is_some() {
        return Err(MigrationError::new("this project is poisoned"));
    }
    project["settings"] = json!({"theme": "dark"});
    Ok(())
}
register_migration!(3, "add_settings", add_settings);

const PROJECT_V1: &str = "# my show\nname = \"Show\" # display name\nschema = 1\n";
const FACE: &str = "# keep this\nprofile = \"round\"\n";

fn old_project() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "project.toml", PROJECT_V1);
    write(tmp.path(), "characters/mia/face.toml", FACE);
    write(tmp.path(), "characters/mia/look.toml", "outline = 2\ncolor = \"red\"\n");
    tmp
}

#[test]
fn new_projects_start_at_the_latest_schema() {
    assert_eq!(supported_schema(), 3);
    let tmp = TempDir::new().unwrap();
    let project = Project::create(tmp.path(), "New").unwrap();
    assert_eq!(project.schema_version(), Some(3));
    assert_eq!(read(tmp.path(), "project.toml"), "name = \"New\"\nschema = 3\n");
    assert_eq!(project.upgraded_from(), None);
}

#[test]
fn an_old_project_is_upgraded_in_memory_and_saved_on_request() {
    let tmp = old_project();
    let dir = tmp.path();
    let on_disk = snapshot(dir);

    let mut project = Project::open(dir).unwrap();
    assert_eq!(project.upgraded_from(), Some(1));
    assert_eq!(project.schema_version(), Some(3));
    assert_eq!(project.get(&common::p("characters/mia/look")), Some(&json!({"outline_width": 2, "color": "red"})));
    assert_eq!(project.get(&common::p("settings/theme")), Some(&json!("dark")));
    assert!(project.is_dirty());
    assert_eq!(snapshot(dir), on_disk, "opening must not write anything");

    // Only the files the migrations changed are written; the rest keep their exact bytes.
    assert_eq!(project.save().unwrap(), SaveReport { written: 3, removed: 0 });
    assert_eq!(read(dir, "characters/mia/face.toml"), FACE);
    assert_eq!(read(dir, "project.toml"), "# my show\nname = \"Show\" # display name\nschema = 3\n");
    assert_eq!(read(dir, "settings.toml"), "theme = \"dark\"\n");
    assert!(read(dir, "characters/mia/look.toml").contains("outline_width = 2"));

    let reopened = Project::open(dir).unwrap();
    assert_eq!(reopened.upgraded_from(), None);
    assert!(!reopened.is_dirty());
    assert_eq!(reopened.root(), project.root());
}

#[test]
fn only_the_missing_migrations_run() {
    let tmp = old_project();
    write(tmp.path(), "project.toml", "name = \"Show\"\nschema = 2\n");
    let project = Project::open(tmp.path()).unwrap();
    assert_eq!(project.upgraded_from(), Some(2));
    assert_eq!(project.get(&common::p("characters/mia/look/outline")), Some(&json!(2)), "schema 2 already had it");
    assert!(project.get(&common::p("settings")).is_some());
}

#[test]
fn a_project_from_the_future_is_refused() {
    let tmp = old_project();
    write(tmp.path(), "project.toml", "name = \"Show\"\nschema = 4\n");
    let err = Project::open(tmp.path()).unwrap_err();
    assert!(matches!(err, StoreError::SchemaTooNew { found: 4, supported: 3 }), "{err}");
}

#[test]
fn a_failing_migration_names_itself_and_leaves_the_folder_alone() {
    let tmp = old_project();
    write(tmp.path(), "project.toml", "name = \"Show\"\nschema = 1\npoison = true\n");
    let on_disk = snapshot(tmp.path());
    let err = Project::open(tmp.path()).unwrap_err();
    assert!(matches!(err, StoreError::Migration { name: "add_settings", to: 3, .. }), "{err}");
    let message = err.to_string();
    assert!(
        message.contains("add_settings") && message.contains("schema 2 to 3") && message.contains("poisoned"),
        "{message}"
    );
    assert_eq!(snapshot(tmp.path()), on_disk);
}
