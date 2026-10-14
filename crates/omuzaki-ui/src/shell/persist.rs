//! Layouts belong to the person, not the project (`shell.md` section 4, rule 6): one TOML file per workspace in the
//! user's config directory, `layouts/<workspace>.toml`.
//!
//! Writing is atomic (a temporary file, then a rename), so a crash never leaves half a layout. Reading never fails the
//! app: a missing, unreadable or corrupt file is reported as such and the caller falls back to the default layout.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::layout::Layout;

/// Why a layout could not be read or written.
#[derive(Debug)]
pub enum StoreError {
    /// The file or folder could not be read or written.
    Io(std::io::Error),
    /// The file is not a valid layout.
    Corrupt(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "layout file: {e}"),
            Self::Corrupt(why) => write!(f, "layout file is not valid: {why}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// The folder layouts are kept in.
#[derive(Clone, Debug)]
pub struct LayoutStore {
    dir: PathBuf,
}

impl LayoutStore {
    /// A store in `dir` (`.../omuzaki/layouts`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store in the user's config directory: `$XDG_CONFIG_HOME/omuzaki/layouts`, else `~/.config/omuzaki/layouts`, and
    /// `%APPDATA%\omuzaki\layouts` on Windows. `None` when none of those is known (the layout then lives only in memory).
    pub fn in_user_config() -> Option<Self> {
        let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let base = if cfg!(windows) {
            env("APPDATA")
        } else {
            env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|home| home.join(".config")))
        }?;
        Some(Self::new(base.join("omuzaki").join("layouts")))
    }

    fn file(&self, workspace: &str) -> PathBuf {
        // A workspace id is a registered `&'static str`, but guard the file name anyway.
        let safe: String = workspace
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe}.toml"))
    }

    /// The saved layout of `workspace`, or `Ok(None)` when none was saved.
    ///
    /// # Errors
    /// [`StoreError`] for an unreadable or corrupt file; the caller uses the default layout.
    pub fn load(&self, workspace: &str) -> Result<Option<Layout>, StoreError> {
        let text = match fs::read_to_string(self.file(workspace)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::Io(e)),
        };
        toml::from_str(&text).map(Some).map_err(|e| StoreError::Corrupt(e.to_string()))
    }

    /// Saves `layout` for `workspace`.
    ///
    /// # Errors
    /// [`StoreError::Io`] when the folder or file cannot be written.
    pub fn save(&self, workspace: &str, layout: &Layout) -> Result<(), StoreError> {
        let text = toml::to_string_pretty(layout).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        fs::create_dir_all(&self.dir).map_err(StoreError::Io)?;
        let target = self.file(workspace);
        let temp = target.with_extension("toml.tmp");
        let mut file = fs::File::create(&temp).map_err(StoreError::Io)?;
        file.write_all(text.as_bytes()).map_err(StoreError::Io)?;
        file.sync_all().map_err(StoreError::Io)?;
        fs::rename(&temp, &target).map_err(StoreError::Io)
    }

    /// Forgets the saved layout of `workspace` (Reset layout). Nothing saved is not an error.
    ///
    /// # Errors
    /// [`StoreError::Io`] when the file exists and cannot be removed.
    pub fn reset(&self, workspace: &str) -> Result<(), StoreError> {
        match fs::remove_file(self.file(workspace)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::super::layout::{Dir, Node};
    use super::*;

    fn sample() -> Layout {
        Layout::new(Node::split(
            Dir::Horizontal,
            vec![Node::tabs(&["outliner", "library"]), Node::pane("viewport")],
            &[300.0, 900.0],
        ))
    }

    fn store() -> (tempfile::TempDir, LayoutStore) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = LayoutStore::new(dir.path().join("layouts"));
        (dir, store)
    }

    #[test]
    fn a_saved_layout_is_read_back_exactly_and_a_missing_one_is_none() {
        let (_keep, store) = store();
        assert!(matches!(store.load("build"), Ok(None)));
        store.save("build", &sample()).expect("saves");
        assert_eq!(store.load("build").expect("reads"), Some(sample()));
        assert!(matches!(store.load("face"), Ok(None)), "another workspace has its own file");
    }

    #[test]
    fn the_file_is_toml_named_for_the_workspace_and_leaves_no_temporary_file() {
        let (_keep, store) = store();
        store.save("build", &sample()).expect("saves");
        let names: Vec<_> = fs::read_dir(store.dir()).expect("dir").flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["build.toml"]);
        let text = fs::read_to_string(store.dir().join("build.toml")).expect("file");
        assert!(text.contains("version = 1") && text.contains("kind = \"split\""), "{text}");
    }

    #[test]
    fn a_corrupt_file_is_an_error_the_caller_can_step_over_not_a_panic() {
        let (_keep, store) = store();
        fs::create_dir_all(store.dir()).expect("dir");
        fs::write(store.dir().join("build.toml"), "this is = not [a layout").expect("write");
        assert!(matches!(store.load("build"), Err(StoreError::Corrupt(_))));
        fs::write(store.dir().join("face.toml"), "version = 1\n[root]\nkind = \"nonsense\"\n").expect("write");
        assert!(matches!(store.load("face"), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn reset_forgets_the_saved_layout_and_forgetting_nothing_is_fine() {
        let (_keep, store) = store();
        store.reset("build").expect("nothing to forget");
        store.save("build", &sample()).expect("saves");
        store.reset("build").expect("forgets");
        assert!(matches!(store.load("build"), Ok(None)));
    }

    #[test]
    fn a_workspace_id_cannot_climb_out_of_the_folder() {
        let (_keep, store) = store();
        store.save("../../etc/passwd", &sample()).expect("saves under a safe name");
        let names: Vec<_> = fs::read_dir(store.dir()).expect("dir").flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, [std::ffi::OsString::from("______etc_passwd.toml")]);
    }
}
