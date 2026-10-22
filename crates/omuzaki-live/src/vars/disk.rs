//! [`DiskVariables`]: values files on disk, as TOML, written atomically.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use omuzaki_live_api::ids::VarName;
use omuzaki_live_api::services::{PersistScope, VarStoreError, VariableStore};
use serde_json::Value;

/// The values files of this machine: a show's under its project folder, the globals under the data folder.
#[derive(Clone, Debug)]
pub struct DiskVariables {
    data: PathBuf,
}

impl DiskVariables {
    /// Globals under `data` (the app's data folder): `<data>/automation/globals.toml`.
    pub fn new(data: impl Into<PathBuf>) -> Self {
        Self { data: data.into() }
    }

    /// Where `scope`'s values live.
    pub fn path(&self, scope: &PersistScope) -> PathBuf {
        match scope {
            PersistScope::Show(key) => {
                Path::new(&key.project).join(".omuzaki").join("automation").join(key.show.as_str()).join("values.toml")
            }
            // `PersistScope` is non-exhaustive; anything new that isn't a show's is the user's.
            _ => self.data.join("automation").join("globals.toml"),
        }
    }
}

/// `values` as a TOML document; JSON nulls (which TOML can't hold) are left out.
fn to_toml(values: &BTreeMap<VarName, Value>) -> Result<String, VarStoreError> {
    let mut table = toml::Table::new();
    for (name, value) in values {
        if value.is_null() {
            continue;
        }
        let value = toml::Value::try_from(value).map_err(|e| VarStoreError::Format(e.to_string()))?;
        table.insert(name.as_str().to_owned(), value);
    }
    toml::to_string(&table).map_err(|e| VarStoreError::Format(e.to_string()))
}

/// The values in a TOML document; keys that can't be variable names are skipped.
fn from_toml(text: &str) -> Result<BTreeMap<VarName, Value>, VarStoreError> {
    let table: toml::Table = toml::from_str(text).map_err(|e| VarStoreError::Format(e.to_string()))?;
    let mut values = BTreeMap::new();
    for (key, value) in table {
        let Ok(name) = VarName::new(key) else { continue };
        let value = serde_json::to_value(value).map_err(|e| VarStoreError::Format(e.to_string()))?;
        values.insert(name, value);
    }
    Ok(values)
}

/// Writes `bytes` to `path` through a temporary file in the same folder and a rename, so a reader (or a crash) sees
/// the old file or the new one, never half of one. `before_rename` runs between the two (a test's failure point).
pub(crate) fn write_atomic(
    path: &Path,
    bytes: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("the values file has no folder"))?;
    // Not an op's file access: the engine's own state under the project's `.omuzaki/` and the data folder (design D10).
    #[allow(clippy::disallowed_methods)]
    std::fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    before_rename()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

impl VariableStore for DiskVariables {
    fn load(&self, scope: &PersistScope) -> Result<BTreeMap<VarName, Value>, VarStoreError> {
        // Not an op's file access: the engine's own state under the project's `.omuzaki/` and the data folder.
        #[allow(clippy::disallowed_methods)]
        let read = std::fs::read_to_string(self.path(scope));
        match read {
            Ok(text) => from_toml(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(VarStoreError::Io(e.to_string())),
        }
    }

    fn save(&self, scope: &PersistScope, values: &BTreeMap<VarName, Value>) -> Result<(), VarStoreError> {
        let text = to_toml(values)?;
        write_atomic(&self.path(scope), text.as_bytes(), || Ok(())).map_err(|e| VarStoreError::Io(e.to_string()))
    }
}

#[cfg(test)]
// The tests write and list fixture files in a temporary folder.
#[allow(clippy::disallowed_methods)]
mod tests {
    use omuzaki_live_api::ids::ShowId;
    use omuzaki_live_api::services::ShowKey;
    use serde_json::json;

    use super::*;

    fn name(s: &str) -> VarName {
        VarName::new(s).unwrap_or_else(|e| panic!("{e}"))
    }

    fn folder() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn values_round_trip_and_nothing_stored_is_empty() {
        let dir = folder();
        let store = DiskVariables::new(dir.path().join("data"));
        let show = PersistScope::Show(ShowKey::new(
            dir.path().join("project").to_string_lossy(),
            ShowId::new("main").unwrap_or_else(|e| panic!("{e}")),
        ));
        assert_eq!(store.load(&show), Ok(BTreeMap::new()));
        let values = BTreeMap::from([
            (name("bonks"), json!(3)),
            (name("ratio"), json!(0.5)),
            (name("live"), json!(true)),
            (name("greeting"), json!("hi \"there\"\nnext")),
            (name("names"), json!(["Kiri", "Mochi"])),
        ]);
        store.save(&show, &values).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(store.load(&show), Ok(values));
        assert!(store.path(&show).ends_with(".omuzaki/automation/main/values.toml"));
        assert!(store.path(&PersistScope::Global).ends_with("data/automation/globals.toml"));
    }

    #[test]
    fn a_damaged_file_is_a_format_error() {
        let dir = folder();
        let store = DiskVariables::new(dir.path());
        let path = store.path(&PersistScope::Global);
        std::fs::create_dir_all(path.parent().unwrap_or(dir.path())).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&path, "bonks = = 3").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(store.load(&PersistScope::Global), Err(VarStoreError::Format(_))));
    }

    #[test]
    fn a_failure_between_the_temporary_write_and_the_rename_leaves_the_old_file() {
        let dir = folder();
        let store = DiskVariables::new(dir.path());
        let old = BTreeMap::from([(name("bonks"), json!(3))]);
        store.save(&PersistScope::Global, &old).unwrap_or_else(|e| panic!("{e}"));
        let path = store.path(&PersistScope::Global);
        let new = to_toml(&BTreeMap::from([(name("bonks"), json!(4))])).unwrap_or_else(|e| panic!("{e}"));
        let failed = write_atomic(&path, new.as_bytes(), || Err(io::Error::other("power cut")));
        assert!(failed.is_err());
        assert_eq!(store.load(&PersistScope::Global), Ok(old));
        // And the temporary file went with the failure.
        let left = std::fs::read_dir(path.parent().unwrap_or(dir.path())).map_or(0, Iterator::count);
        assert_eq!(left, 1);
    }
}
