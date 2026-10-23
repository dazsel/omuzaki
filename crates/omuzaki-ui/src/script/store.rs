//! My scripts on disk (#1097, `scripting.md` section 7): the person's own scripts and macros, kept outside any project
//! so they travel across projects like layouts. A script is `<id>.rhai` and a macro `<id>.macro.jsonl` in one folder
//! (`<config dir>/omuzaki/scripts`); a script's name, which a `.rhai` file can't hold without changing its hash, is kept
//! in `names.json` beside them. The `script.save_mine`, `script.delete_mine` and `script.list` ops reach it as the
//! executor's [`ScriptStore`] service.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use omuzaki_ops_api::OpError;
use omuzaki_script::{ScriptKind, ScriptStore, StoredScript, content_hash};

/// The file ending of a script.
const SCRIPT: &str = ".rhai";
/// The file ending of a macro.
const MACRO: &str = ".macro.jsonl";
/// The file of the scripts' names.
const NAMES: &str = "names.json";

/// My scripts in a folder.
#[derive(Debug)]
pub struct DirScriptStore {
    dir: PathBuf,
    /// Held while a write is under way, so two saves never interleave their files and the names.
    writing: Mutex<()>,
}

impl DirScriptStore {
    /// My scripts in `dir`, made when the first one is saved.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), writing: Mutex::new(()) }
    }

    /// My scripts in the person's config folder, when the OS gives one.
    pub fn in_user_config() -> Option<Self> {
        crate::shell::persist::user_config_dir().map(|dir| Self::new(dir.join("scripts")))
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn names(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(self.dir.join(NAMES))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write_names(&self, names: &BTreeMap<String, String>) -> Result<(), OpError> {
        let text = serde_json::to_string_pretty(names).map_err(|e| OpError::internal(e.to_string()))?;
        write(&self.dir.join(NAMES), &text)
    }

    fn file(&self, id: &str, kind: ScriptKind) -> Result<PathBuf, OpError> {
        let safe =
            !id.is_empty() && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if !safe {
            return Err(OpError::invalid("id", format!("`{id}` can't name a file in My scripts"))
                .hint("use lower-case letters, digits and dashes"));
        }
        let ending = match kind {
            ScriptKind::Script => SCRIPT,
            ScriptKind::Macro => MACRO,
        };
        Ok(self.dir.join(format!("{id}{ending}")))
    }
}

impl ScriptStore for DirScriptStore {
    fn list(&self) -> Result<Vec<StoredScript>, OpError> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Ok(Vec::new()) };
        let names = self.names();
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            let (id, kind) = if let Some(id) = file.strip_suffix(MACRO) {
                (id.to_owned(), ScriptKind::Macro)
            } else if let Some(id) = file.strip_suffix(SCRIPT) {
                (id.to_owned(), ScriptKind::Script)
            } else {
                continue;
            };
            let Ok(body) = std::fs::read_to_string(entry.path()) else { continue };
            let name = match kind {
                ScriptKind::Script => names.get(&id).cloned().unwrap_or_else(|| id.clone()),
                ScriptKind::Macro => omuzaki_script::from_jsonl(&body).map_or_else(|_| id.clone(), |m| m.name),
            };
            let hash = content_hash(&body);
            out.push(StoredScript { id, name, kind, body, hash });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn get(&self, id: &str) -> Result<Option<StoredScript>, OpError> {
        Ok(self.list()?.into_iter().find(|s| s.id == id))
    }

    fn put(&self, script: StoredScript) -> Result<(), OpError> {
        let _writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let file = self.file(&script.id, script.kind)?;
        write(&file, &script.body)?;
        // One id is one item: saving a script under a macro's id replaces the macro.
        let other = match script.kind {
            ScriptKind::Script => ScriptKind::Macro,
            ScriptKind::Macro => ScriptKind::Script,
        };
        let _ = std::fs::remove_file(self.file(&script.id, other)?);
        let mut names = self.names();
        match script.kind {
            ScriptKind::Script => names.insert(script.id.clone(), script.name),
            ScriptKind::Macro => names.remove(&script.id),
        };
        self.write_names(&names)
    }

    fn remove(&self, id: &str) -> Result<bool, OpError> {
        let _writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let mut removed = false;
        for kind in [ScriptKind::Script, ScriptKind::Macro] {
            removed |= std::fs::remove_file(self.file(id, kind)?).is_ok();
        }
        let mut names = self.names();
        if names.remove(id).is_some() {
            self.write_names(&names)?;
        }
        Ok(removed)
    }
}

/// Writes `text` to `file` whole: to a temporary file first, then renamed over it, so a crash never leaves half a script.
fn write(file: &Path, text: &str) -> Result<(), OpError> {
    let failed = |e: std::io::Error| OpError::internal(format!("My scripts could not be written: {e}"));
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let partial = file.with_extension("partial");
    std::fs::write(&partial, text).map_err(failed)?;
    std::fs::rename(&partial, file).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, name: &str, kind: ScriptKind, body: &str) -> StoredScript {
        StoredScript { id: id.into(), name: name.into(), kind, body: body.into(), hash: content_hash(body) }
    }

    #[test]
    fn scripts_and_macros_keep_their_names_and_bodies_across_opens() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let store = DirScriptStore::new(dir.path().join("scripts"));
        assert!(store.list().unwrap_or_default().is_empty(), "no folder yet is an empty list");
        store.put(item("tidy", "Tidy the fringe", ScriptKind::Script, "print(1);")).unwrap_or_else(|e| panic!("{e}"));
        let jsonl = omuzaki_script::to_jsonl(&omuzaki_script::Macro { name: "Blush".into(), steps: Vec::new() });
        store.put(item("blush", "Blush", ScriptKind::Macro, &jsonl)).unwrap_or_else(|e| panic!("{e}"));

        let again = DirScriptStore::new(dir.path().join("scripts"));
        let listed = again.list().unwrap_or_else(|e| panic!("{e}"));
        let named: Vec<(&str, &str, ScriptKind)> =
            listed.iter().map(|s| (s.id.as_str(), s.name.as_str(), s.kind)).collect();
        assert_eq!(named, [("blush", "Blush", ScriptKind::Macro), ("tidy", "Tidy the fringe", ScriptKind::Script)]);
        assert_eq!(listed[1].hash, content_hash("print(1);"));

        assert!(again.remove("tidy").unwrap_or_default());
        assert!(!again.remove("tidy").unwrap_or_default());
        assert_eq!(again.list().unwrap_or_default().len(), 1);
    }

    #[test]
    fn an_id_that_isnt_a_file_name_is_refused() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let store = DirScriptStore::new(dir.path());
        for id in ["../escape", "a/b", "", "C:x"] {
            assert!(store.put(item(id, "x", ScriptKind::Script, "1")).is_err(), "{id}");
        }
    }
}
