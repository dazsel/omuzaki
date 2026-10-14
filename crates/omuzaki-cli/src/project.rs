//! The document the executor works on: a project folder that saves itself after every change.
//!
//! [`omuzaki_doc::Project`] keeps the document in memory and writes it only when told to, and the executor owns the
//! document it is given, so nothing outside could tell it when to. [`SavedProject`] is the CLI's answer: it wraps the
//! project behind the [`DocWrite`] seam and saves after each applied patch, so `omuzaki run` leaves the files changed
//! and an MCP session's edits reach the disk as the agent makes them. A file save only rewrites the files whose part
//! of the document changed, so this costs one small write for a typical op. Saving belongs to the store, and this
//! wrapper goes when `omuzaki-doc` can do it itself.
//!
//! [`NoProject`] is the empty document for commands that only read the op registry (`ops`, `help`).

use std::path::Path;

use omuzaki_doc::{Project, StoreError};
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, Value};
use omuzaki_ops_api::OpError;

/// A [`Project`] that is saved to disk after every patch it applies.
#[derive(Debug)]
pub struct SavedProject {
    project: Project,
}

impl SavedProject {
    /// Opens the project in `dir`.
    ///
    /// # Errors
    /// `NotFound` if there is no project there; `Unsupported` if it was written by a newer Omuzaki; `Invalid` (field
    /// `project`) if its files are broken; `Internal` if the store reports a bug of its own.
    pub fn open(dir: &Path) -> Result<Self, OpError> {
        Project::open(dir).map(|project| Self { project }).map_err(|e| open_error(dir, &e))
    }

    /// Takes a patch that was applied but could not be saved back out, so the document and the files agree again.
    /// Returns the error to report: what failed, and whether the files are known to match.
    fn undo_unsaved(&mut self, patch: &Patch, save_error: &StoreError) -> PatchError {
        let reason = match self.project.apply(&patch.inverse()) {
            Err(e) => format!(
                "couldn't save the project ({save_error}) and couldn't take the change back out ({e}); \
                 reopen the project"
            ),
            // The failed save may have written some of the files: write them again from the restored document.
            Ok(_) => match self.project.save() {
                Ok(_) => format!("couldn't save the project ({save_error}); the change was not made"),
                Err(again) => {
                    format!("couldn't save the project ({save_error}), and the files may not match it: {again}")
                }
            },
        };
        PatchError::Unreachable { path: first_path(patch), reason }
    }
}

impl DocRead for SavedProject {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        self.project.get(path)
    }

    fn version(&self) -> DocVersion {
        self.project.version()
    }
}

impl DocWrite for SavedProject {
    /// Applies `patch`, then saves. If the save fails the patch is taken back out, so a refused change leaves the
    /// document as it was.
    ///
    /// # Errors
    /// The project's own conflicts, and [`PatchError::Unreachable`] carrying the reason when the save fails
    /// (`PatchError` has no variant for a failed save yet).
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError> {
        let version = self.project.apply(patch)?;
        match self.project.save() {
            Ok(_) => Ok(version),
            Err(e) => Err(self.undo_unsaved(patch, &e)),
        }
    }
}

/// The document of a session with no project: empty, and it refuses every change. Commands that would change or read
/// a project never get this far (see [`session::open_runner`](crate::session::open_runner)).
#[derive(Debug, Default)]
pub struct NoProject;

impl DocRead for NoProject {
    fn get(&self, _path: &DocPath) -> Option<&Value> {
        None
    }

    fn version(&self) -> DocVersion {
        DocVersion(0)
    }
}

impl DocWrite for NoProject {
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError> {
        Err(PatchError::Unreachable { path: first_path(patch), reason: "no project is open".to_owned() })
    }
}

/// Where to point a [`PatchError::Unreachable`] about a whole patch: its first change, or the root for an empty one.
fn first_path(patch: &Patch) -> DocPath {
    patch.changes.first().map_or_else(DocPath::root, |change| change.path.clone())
}

/// Words a failure to open the project in `dir` as the structured error the CLI reports.
fn open_error(dir: &Path, error: &StoreError) -> OpError {
    let message = error.to_string();
    match error {
        StoreError::NotAProject { .. } => {
            OpError::not_found(message).hint("pass the folder that holds project.toml: --project <DIR>")
        }
        StoreError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            OpError::not_found(format!("{} does not exist, or a file in it is missing", dir.display()))
                .hint(format!("{message}; pass the project folder: --project <DIR>"))
        }
        StoreError::SchemaTooNew { .. } => OpError::unsupported(message)
            .hint("open the project with the Omuzaki version that wrote it, or update this one"),
        // The store's own bookkeeping or migration table is wrong: nothing the user can fix in the project.
        StoreError::Internal(_) | StoreError::MissingMigration { .. } | StoreError::DuplicateMigration { .. } => {
            OpError::internal(message).hint("this is a bug in Omuzaki: report it")
        }
        // Unreadable or malformed files, a file and a folder with one name, a failing migration, anything new.
        _ => OpError::invalid("project", message)
            .hint("fix the file named above (or restore it from version control), then run the command again"),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]
mod tests {
    use omuzaki_doc_api::DocPath;
    use omuzaki_ops_api::ErrorCode;
    use serde_json::json;

    use super::*;

    fn path(p: &str) -> DocPath {
        DocPath::parse(p).unwrap()
    }

    fn new_project() -> (tempfile::TempDir, SavedProject) {
        let dir = tempfile::tempdir().unwrap();
        Project::create(dir.path(), "demo").unwrap();
        let project = SavedProject::open(dir.path()).unwrap();
        (dir, project)
    }

    fn set_note(project: &SavedProject, note: Option<&str>) -> Patch {
        let at = path("project/note");
        Patch::new().with(at.clone(), project.get(&at).cloned(), note.map(|n| json!(n)))
    }

    fn project_toml(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("project.toml")).unwrap()
    }

    #[test]
    fn every_applied_patch_is_on_disk_when_apply_returns() {
        let (dir, mut project) = new_project();
        let patch = set_note(&project, Some("hello"));
        project.apply(&patch).unwrap();
        assert!(project_toml(&dir).contains("note = \"hello\""), "{}", project_toml(&dir));

        project.apply(&patch.inverse()).unwrap();
        assert!(!project_toml(&dir).contains("note"), "{}", project_toml(&dir));
        assert!(Project::open(dir.path()).unwrap().get(&path("project/note")).is_none());
    }

    #[test]
    fn a_conflicting_patch_changes_nothing_and_writes_nothing() {
        let (dir, mut project) = new_project();
        let before = project_toml(&dir);
        let stale = Patch::new().with(path("project/note"), Some(json!("not there")), Some(json!("x")));
        assert!(matches!(project.apply(&stale), Err(PatchError::Conflict { .. })));
        assert_eq!(project_toml(&dir), before);
        assert!(project.get(&path("project/note")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_takes_the_change_back_out_and_says_why() {
        let (dir, mut project) = new_project();
        // The store refuses to write through or over a symbolic link.
        let real = dir.path().join("elsewhere.toml");
        std::fs::rename(dir.path().join("project.toml"), &real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("project.toml")).unwrap();

        let patch = set_note(&project, Some("hello"));
        let Err(PatchError::Unreachable { path: at, reason }) = project.apply(&patch) else {
            panic!("the save should have failed");
        };
        assert_eq!(at, path("project/note"));
        assert!(reason.contains("symbolic link") && reason.contains("the change was not made"), "{reason}");
        assert!(project.get(&path("project/note")).is_none(), "the document must not keep an unsaved change");
        assert!(!std::fs::read_to_string(&real).unwrap().contains("note"));
    }

    #[test]
    fn opening_failures_are_structured_errors_with_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        let code = |dir: &Path| SavedProject::open(dir).unwrap_err();

        let empty = code(dir.path());
        assert_eq!(empty.code, ErrorCode::NotFound, "{empty:?}");
        assert!(empty.hint.is_some_and(|h| h.contains("--project")));

        let missing = code(&dir.path().join("nope"));
        assert_eq!(missing.code, ErrorCode::NotFound, "{missing:?}");

        std::fs::write(dir.path().join("project.toml"), "name = ").unwrap();
        let broken = code(dir.path());
        assert_eq!((broken.code, broken.field.as_deref()), (ErrorCode::Invalid, Some("project")), "{broken:?}");
        assert!(broken.message.contains("project.toml"), "{broken:?}");

        std::fs::write(dir.path().join("project.toml"), "name = \"x\"\nschema = 9999\n").unwrap();
        let newer = code(dir.path());
        assert_eq!(newer.code, ErrorCode::Unsupported, "{newer:?}");
        assert!(newer.message.contains("9999"), "{newer:?}");
    }

    #[test]
    fn no_project_is_empty_and_refuses_changes() {
        let mut doc = NoProject;
        assert!(doc.get(&path("project/name")).is_none());
        let patch = Patch::new().with(path("project/name"), None, Some(json!("x")));
        assert!(matches!(doc.apply(&patch), Err(PatchError::Unreachable { .. })));
        assert_eq!(doc.version(), DocVersion(0));
    }
}
