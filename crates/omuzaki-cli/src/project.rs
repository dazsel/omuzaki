//! What the executor works on: a project opened through the app core, or no project at all.
//!
//! [`ProjectRunner`] opens a project the way `omuzaki run` and `omuzaki mcp` do ([`omuzaki_app::AppCore`]): every
//! change is journalled before it is answered, a crash is recovered on the next open, and the undo and redo history
//! is kept with the project. On top of the core it writes the files before each change returns, so a host that reads
//! them straight after (a test, the eval checker) sees it; the app core alone writes them within about two seconds.
//!
//! [`NoProject`] is the empty document for commands that only read the op registry (`ops`, `help`).

use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use omuzaki_app::{AppCore, AppError, AppOptions};
use omuzaki_doc::{Project, StoreError};
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, Value};
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, TxId};

/// How long a change waits for the files.
const SAVE_WAIT: Duration = Duration::from_secs(30);

/// A runner over a project opened through the app core, which writes the files before each change returns and
/// closes the project cleanly when it is dropped (or [`close`](Self::close)d).
pub struct ProjectRunner {
    core: AppCore,
}

impl std::fmt::Debug for ProjectRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectRunner").field("dir", &self.core.dir()).finish_non_exhaustive()
    }
}

impl ProjectRunner {
    /// Opens the project in `dir` through the app core with `options`.
    ///
    /// # Errors
    /// As [`AppCore::open`]: not a project, open in another Omuzaki, broken files, a journal that can't be written.
    pub fn open(dir: &Path, options: AppOptions) -> Result<Self, AppError> {
        AppCore::open(dir, options).map(|core| Self { core })
    }

    /// The app core: its session registry, jobs, MCP server and what recovery did on open.
    pub fn core(&self) -> &AppCore {
        &self.core
    }

    /// Closes the project: the files are written and the close snapshot taken, so the next open needs no recovery.
    ///
    /// # Errors
    /// Saving failed or timed out. Nothing applied is lost: the next open recovers it from the journal.
    pub fn close(self) -> Result<(), OpError> {
        self.core.close().map_err(|e| e.to_op_error())
    }

    /// Waits for the files after a change that was applied.
    fn saved<T>(&self, result: Result<T, OpError>) -> Result<T, OpError> {
        let value = result?;
        self.core.snapshots().save_files(SAVE_WAIT).map_err(|e| {
            OpError::invalid(
                "project",
                format!("the change was made and journalled, but the files weren't written: {e}"),
            )
            .hint("check the disk has space and the project folder can be written; the next open writes them")
        })?;
        Ok(value)
    }
}

impl Runner for ProjectRunner {
    fn ops(&self) -> Vec<OpInfo> {
        self.core.executor().ops()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let outcome = self.core.executor().run(req)?;
        if outcome.applied.is_some() { self.saved(Ok(outcome)) } else { Ok(outcome) }
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.core.executor().begin(label, actor)
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.saved(self.core.executor().commit(tx, actor))
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.saved(self.core.executor().rollback(tx, actor))
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.saved(self.core.executor().undo(actor, grants))
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.saved(self.core.executor().redo(actor, grants))
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.core.executor().subscribe(grants)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.core.executor().get(path, grants)
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.core.executor().log(after_seq, limit, grants)
    }
}

/// Opens the project in `dir` as it is on disk, in memory: an executor over it reads and plans as on the real
/// project, and nothing it applies is written. A host that saves opens a [`ProjectRunner`].
///
/// # Errors
/// `NotFound` if there is no project there; `Unsupported` if it was written by a newer Omuzaki; `Invalid` (field
/// `project`) if its files are broken; `Internal` if the store reports a bug of its own.
pub fn open_in_memory(dir: &Path) -> Result<Project, OpError> {
    Project::open(dir).map_err(|e| open_error(dir, &e))
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
    use omuzaki_ops_api::{ErrorCode, Mode};
    use serde_json::json;

    use super::*;

    fn path(p: &str) -> DocPath {
        DocPath::parse(p).unwrap()
    }

    fn person() -> Actor {
        Actor::Person { name: "tester".to_owned() }
    }

    fn create_kit(runner: &ProjectRunner) -> Outcome {
        let args = json!({ "id": "kit", "name": "Kit", "template": "standard", "seed": 7 });
        let request = Request {
            op: "character.create".to_owned(),
            args,
            mode: Mode::Apply,
            actor: person(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        runner.run(request).unwrap()
    }

    #[test]
    fn every_applied_change_and_undo_is_on_disk_when_it_returns_and_history_outlives_the_runner() {
        crate::link();
        let dir = tempfile::tempdir().unwrap();
        Project::create(dir.path(), "demo").unwrap();
        let kit = dir.path().join("characters/kit.toml");
        let runner = ProjectRunner::open(dir.path(), AppOptions::headless(person())).unwrap();
        assert!(create_kit(&runner).applied.is_some());
        assert!(kit.exists(), "the character's file is written before the run returns");
        runner.undo(&person(), &Grants::person()).unwrap();
        assert!(!kit.exists(), "and the undo's");
        runner.close().unwrap();

        let again = ProjectRunner::open(dir.path(), AppOptions::headless(person())).unwrap();
        assert!(again.redo(&person(), &Grants::person()).unwrap().is_some(), "the redo survived the close");
        assert!(kit.exists());
        assert!(again.get(&path("characters/kit"), &Grants::person()).unwrap().is_some());
    }

    #[test]
    fn opening_failures_are_structured_errors_with_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        let code = |dir: &Path| open_in_memory(dir).unwrap_err();

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
