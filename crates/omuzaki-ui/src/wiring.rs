//! The app's wiring module (QG-11, `cargo xtask lint-ui-writes`): the one place in the UI that builds the op executor
//! and its document, and hands out the [`Runner`]. Everything else in the UI gets the `Runner` passed in and changes
//! the document only through it.
//!
//! The document is a project folder ([`Document::project`]) that is saved after every applied change, so what the
//! person and an agent do survives a quit, or an empty in-memory tree ([`Document::empty`]) for tests and for an app
//! that could not open a folder. The app core's handle replaces this module once it lands (spine map, area 1).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_core::time::SystemClock;
use omuzaki_doc::{Project, SavedProject, StoreError};
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Scratch, Value};
use omuzaki_ops::Executor;
pub use omuzaki_ops::ExecutorError;
use omuzaki_ops_api::{Runner, Services, Sessions};

/// The name a project folder the app makes for itself is given.
const DEFAULT_PROJECT_NAME: &str = "My character";

/// The document the executor works on, opaque outside this module so nothing else can write to it.
pub struct Document {
    doc: Tree,
    dir: Option<PathBuf>,
}

/// What a [`Document`] holds: an in-memory tree, or a project saved after every change.
enum Tree {
    Memory(Scratch),
    Project(SavedProject),
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Document {
    /// An empty in-memory document at version 0: what ops change lasts until the app quits.
    pub fn empty() -> Self {
        Self { doc: Tree::Memory(Scratch::of(&Empty)), dir: None }
    }

    /// The project in `dir`, made there first when the folder holds none. Every applied change is saved at once.
    ///
    /// # Errors
    /// [`StoreError`] when the folder can't be read or made, or its files are broken.
    pub fn project(dir: &Path) -> Result<Self, StoreError> {
        let project = match Project::open(dir) {
            Ok(project) => project,
            Err(StoreError::NotAProject { .. }) => Project::create(dir, DEFAULT_PROJECT_NAME)?,
            Err(StoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Project::create(dir, DEFAULT_PROJECT_NAME)?
            }
            Err(error) => return Err(error),
        };
        Ok(Self { doc: Tree::Project(SavedProject::new(project)), dir: Some(dir.to_path_buf()) })
    }

    /// The project folder, when the document is one.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The value at `path`, read before an executor takes the document (the app reads what it stages first).
    pub fn get(&self, path: &DocPath) -> Option<&Value> {
        match &self.doc {
            Tree::Memory(tree) => tree.get(path),
            Tree::Project(project) => project.get(path),
        }
    }
}

/// The folder the app keeps its own project in when none is named: `$XDG_DATA_HOME/omuzaki/studio`, else
/// `~/.local/share/omuzaki/studio`, and `%APPDATA%\omuzaki\studio` on Windows. `None` when none of those is known.
pub fn default_project_dir() -> Option<PathBuf> {
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("APPDATA")
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|home| home.join(".local").join("share")))
    }?;
    Some(base.join("omuzaki").join("studio"))
}

/// The runner of an app with no project open: every op linked into the app, over an empty in-memory document, with
/// `services`.
///
/// # Errors
/// [`ExecutorError`] when two linked crates register one op name (a bug in whichever registered it).
pub fn runner(services: Services) -> Result<Arc<dyn Runner>, ExecutorError> {
    runner_on(Document::empty(), services)
}

/// The runner over `document`: every op linked into the app, with `services`.
///
/// # Errors
/// [`ExecutorError`] when two linked crates register one op name (a bug in whichever registered it).
pub fn runner_on(document: Document, services: Services) -> Result<Arc<dyn Runner>, ExecutorError> {
    core_on(document, services).map(|core| core.runner)
}

/// The one executor of the app, seen through the contracts the UI uses: the [`Runner`] every change goes through and
/// the agent [`Sessions`] the Agent panel shows. Both are the same executor, so an agent's session and the ops it runs
/// are one history.
#[derive(Clone)]
pub struct Core {
    /// Runs every op, for the person and for every agent.
    pub runner: Arc<dyn Runner>,
    /// The agent sessions: who is connected, what they may do, pause and stop.
    pub sessions: Arc<dyn Sessions>,
}

impl fmt::Debug for Core {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Core").finish_non_exhaustive()
    }
}

/// The app's executor over `document`: every op linked into the app, with `services`.
///
/// # Errors
/// [`ExecutorError`] when two linked crates register one op name (a bug in whichever registered it).
pub fn core_on(document: Document, services: Services) -> Result<Core, ExecutorError> {
    let clock = Arc::new(SystemClock::new());
    let executor = match document.doc {
        Tree::Memory(tree) => Executor::new(Box::new(tree), clock),
        Tree::Project(project) => Executor::new(Box::new(project), clock),
    }?;
    let executor = Arc::new(executor.with_services(services));
    Ok(Core { runner: executor.clone(), sessions: executor })
}

/// The tree an app with no project open starts from: empty, at version 0. The executor works on an in-memory copy
/// of it ([`Scratch`]), so what ops change lasts until the app quits; opening a project replaces it.
#[derive(Clone, Copy, Debug, Default)]
struct Empty;

impl DocRead for Empty {
    fn get(&self, _path: &DocPath) -> Option<&Value> {
        None
    }

    fn version(&self) -> DocVersion {
        DocVersion(0)
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{Actor, Grants, Mode, Request};
    use serde_json::json;

    use super::*;

    #[test]
    fn a_project_folder_is_made_on_first_open_and_keeps_what_ops_changed() {
        omuzaki_char::link();
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let folder = dir.path().join("show");
        let runner = runner_on(Document::project(&folder).unwrap_or_else(|e| panic!("{e}")), Services::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let request = Request {
            op: "character.create".into(),
            args: json!({"id": "kit", "name": "Kit", "template": "soft-sd"}),
            mode: Mode::Apply,
            actor: Actor::Person { name: "you".into() },
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        runner.run(request).unwrap_or_else(|e| panic!("{e}"));
        drop(runner);
        let reopened = runner_on(Document::project(&folder).unwrap_or_else(|e| panic!("{e}")), Services::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let name = reopened
            .get(&DocPath::parse("characters/kit/name").unwrap_or_else(|e| panic!("{e}")), &Grants::person())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(name, Some(json!("Kit")));
    }
}
