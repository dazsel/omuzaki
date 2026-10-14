//! Schema versions and the migrations that bring old projects up to date (spec §7: "old projects always open").
//!
//! `project.toml` holds `schema = N`. Opening a project older than this build runs one migration per version in
//! between, in order; opening a newer one is an error, because guessing at data we don't understand would lose it.
//!
//! Migrations register themselves with [`register_migration!`](crate::register_migration), one per file in
//! `src/migrations/`, so adding one never edits a shared list (spec §39 rules 2 and 5).

use serde_json::{Map, Value};

use crate::error::StoreError;
use crate::toml_write::check_storable;

/// The schema of the first projects ever written. It is the schema of a project when no migration is registered.
pub const BASE_SCHEMA: u32 = 1;

/// Why a migration failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MigrationError(String);

impl MigrationError {
    /// A failure with a message that tells the user what is wrong with their project.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// One step: turns a project of schema `to - 1` into one of schema `to`.
///
/// Declare it with [`register_migration!`](crate::register_migration); don't build one by hand.
pub struct Migration {
    /// The schema this migration produces. The first one is [`BASE_SCHEMA`] + 1, and they follow with no gaps.
    pub to: u32,
    /// What it does, as in the file name `m<to>_<name>.rs`.
    pub name: &'static str,
    /// The source file that declares it (`file!()`), which a test checks against `to` and `name`.
    pub file: &'static str,
    /// The change. It gets the whole project tree and must not touch `project/schema`: the runner sets it.
    pub up: fn(&mut Value) -> Result<(), MigrationError>,
}

inventory::collect!(Migration);

/// Registers a migration from this crate's `src/migrations/m<NNNN>_<name>.rs`.
///
/// `NNNN` is the schema the migration produces (`to`), written with four digits. A test fails if the number or name
/// in the file name and in the macro disagree, or if two migrations produce the same schema or one is missing.
///
/// Write the migration so that running it again does nothing: `save` writes `project.toml` (and with it the new
/// schema) last, so a save that dies part way leaves the old schema over some files that are already migrated, and
/// the next open runs the migration over that mix. Also, don't add a plain value directly to a folder of files (see
/// the crate docs); put it in one of the files.
///
/// ```ignore
/// // src/migrations/m0002_rename_look_outline.rs
/// use omuzaki_doc::{MigrationError, register_migration};
///
/// fn up(project: &mut serde_json::Value) -> Result<(), MigrationError> { /* ... */ Ok(()) }
///
/// register_migration!(2, "rename_look_outline", up);
/// ```
#[macro_export]
macro_rules! register_migration {
    ($to:expr, $name:expr, $up:path) => {
        $crate::__inventory::submit! {
            $crate::Migration { to: $to, name: $name, file: file!(), up: $up }
        }
        $crate::__record_registration!("migration", $to);
    };
}

/// Every registered migration, oldest first.
pub(crate) fn registered() -> Vec<&'static Migration> {
    let mut all: Vec<&'static Migration> = inventory::iter::<Migration>.into_iter().collect();
    all.sort_by_key(|m| m.to);
    all
}

/// The newest schema that `migrations` produce.
pub(crate) fn latest(migrations: &[&Migration]) -> u32 {
    migrations.iter().map(|m| m.to).max().unwrap_or(BASE_SCHEMA)
}

/// The newest schema this build can open, which is also the schema new projects are created with.
pub fn supported_schema() -> u32 {
    latest(&registered())
}

/// The schema recorded in the project tree.
pub(crate) fn schema_of(root: &Value) -> Result<u32, StoreError> {
    root.get("project")
        .and_then(Value::as_object)
        .map_or_else(|| Err(StoreError::BadSchema("there is no project table".to_owned())), schema_of_project)
}

/// The schema recorded in the contents of `project.toml`.
pub(crate) fn schema_of_project(project: &Map<String, Value>) -> Result<u32, StoreError> {
    match project.get("schema") {
        None => Err(StoreError::BadSchema("there is no `schema = N` line".to_owned())),
        Some(v) => {
            v.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n >= BASE_SCHEMA).ok_or_else(|| {
                StoreError::BadSchema(format!("`schema` must be a whole number from 1 up, but it is {v}"))
            })
        }
    }
}

/// Runs the migrations from schema `from` up to the newest, checking after each that the tree can still be stored.
pub(crate) fn upgrade(root: &mut Value, from: u32, migrations: &[&Migration]) -> Result<(), StoreError> {
    for to in (from + 1)..=latest(migrations) {
        let step = step_to(migrations, to)?;
        (step.up)(root).map_err(|source| StoreError::Migration { name: step.name, to, source })?;
        if let Some(project) = root.get_mut("project").and_then(Value::as_object_mut) {
            project.insert("schema".to_owned(), Value::from(to));
        }
        check_tree(root).map_err(|why| StoreError::MigratedTree(format!("after {}: {why}", step.name)))?;
    }
    Ok(())
}

fn step_to<'a>(migrations: &[&'a Migration], to: u32) -> Result<&'a Migration, StoreError> {
    let mut found = migrations.iter().copied().filter(|m| m.to == to);
    match (found.next(), found.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => Err(StoreError::MissingMigration { to }),
        (Some(_), Some(_)) => Err(StoreError::DuplicateMigration { to }),
    }
}

/// Checks the shape every project tree must have: a `project` entry, objects at the top level, and values TOML can hold.
pub(crate) fn check_tree(root: &Value) -> Result<(), String> {
    let Value::Object(top) = root else { return Err("the project must be an object".to_owned()) };
    if !top.get("project").is_some_and(Value::is_object) {
        return Err("`project` is missing or isn't an object".to_owned());
    }
    for (key, value) in top {
        if !value.is_object() {
            return Err(format!("top-level entry '{key}' isn't an object"));
        }
        check_storable(value).map_err(|u| format!("{} (at {key}/{})", u.reason, u.at))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unnecessary_wraps)] // the test migrations must have the fallible signature of the real ones
mod tests {
    use serde_json::json;

    use super::*;

    fn add_field(root: &mut Value) -> Result<(), MigrationError> {
        root["settings"] = json!({"added": true});
        Ok(())
    }

    fn rename_field(root: &mut Value) -> Result<(), MigrationError> {
        let settings = root["settings"].take();
        root["prefs"] = settings;
        root.as_object_mut().map(|m| m.remove("settings"));
        Ok(())
    }

    fn refuse(_: &mut Value) -> Result<(), MigrationError> {
        Err(MigrationError::new("no good"))
    }

    fn make_null(root: &mut Value) -> Result<(), MigrationError> {
        root["project"]["x"] = Value::Null;
        Ok(())
    }

    fn step(to: u32, up: fn(&mut Value) -> Result<(), MigrationError>) -> Migration {
        Migration { to, name: "test", file: file!(), up }
    }

    fn project(schema: u32) -> Value {
        json!({"project": {"name": "t", "schema": schema}})
    }

    #[test]
    fn migrations_run_in_order_and_the_runner_sets_the_schema() {
        let (a, b) = (step(2, add_field), step(3, rename_field));
        let mut root = project(1);
        upgrade(&mut root, 1, &[&b, &a]).unwrap();
        assert_eq!(root, json!({"project": {"name": "t", "schema": 3}, "prefs": {"added": true}}));
        let mut from_two = project(2);
        from_two["settings"] = json!({"old": 1});
        upgrade(&mut from_two, 2, &[&a, &b]).unwrap();
        assert_eq!(from_two["prefs"], json!({"old": 1}));
    }

    #[test]
    fn a_project_at_the_latest_schema_is_left_alone() {
        let a = step(2, add_field);
        let mut root = project(2);
        upgrade(&mut root, 2, &[&a]).unwrap();
        assert_eq!(root, project(2));
    }

    #[test]
    fn gaps_duplicates_failures_and_bad_results_are_errors() {
        let (a, c) = (step(2, add_field), step(4, add_field));
        assert!(matches!(upgrade(&mut project(1), 1, &[&a, &c]), Err(StoreError::MissingMigration { to: 3 })));
        assert!(matches!(
            upgrade(&mut project(1), 1, &[&a, &step(2, add_field)]),
            Err(StoreError::DuplicateMigration { to: 2 })
        ));
        let err = upgrade(&mut project(1), 1, &[&step(2, refuse)]).unwrap_err();
        assert!(err.to_string().contains("no good") && err.to_string().contains("schema 1 to 2"), "{err}");
        assert!(matches!(upgrade(&mut project(1), 1, &[&step(2, make_null)]), Err(StoreError::MigratedTree(_))));
    }

    #[test]
    fn schema_must_be_a_positive_whole_number() {
        assert_eq!(schema_of(&project(3)).unwrap(), 3);
        for bad in [
            json!({"project": {}}),
            json!({"project": {"schema": 0}}),
            json!({"project": {"schema": "1"}}),
            json!({"project": {"schema": 1.5}}),
            json!({"project": {"schema": -1}}),
        ] {
            assert!(matches!(schema_of(&bad), Err(StoreError::BadSchema(_))), "{bad}");
        }
    }

    /// Catches a number that doesn't match the file name, a gap, or two files claiming one schema. (With the
    /// migrations that ship this passes by being empty; it is the guard for the one that gets added.)
    #[test]
    fn registered_migrations_are_contiguous_and_named_after_their_files() {
        for (i, m) in registered().iter().enumerate() {
            let want = BASE_SCHEMA + 1 + u32::try_from(i).unwrap();
            assert_eq!(m.to, want, "migration {} breaks the sequence", m.name);
            let file = m.file.rsplit(['/', '\\']).next().unwrap();
            assert_eq!(file, format!("m{:04}_{}.rs", m.to, m.name), "file name and registration disagree");
        }
    }
}
