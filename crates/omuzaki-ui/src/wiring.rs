//! The app's wiring module (QG-11, `cargo xtask lint-ui-writes`): the one place in the UI that builds the op executor
//! and its document, and hands out the [`Runner`]. Everything else in the UI gets the `Runner` passed in and changes
//! the document only through it.
//!
//! The document is a project folder ([`Document::project`]), opened through the app core ([`AppCore`]): the folder's
//! lock, crash recovery, the journal that makes every applied change durable within milliseconds, every actor's undo
//! and redo across restarts, and the MCP server on the same executor, so an agent's op shows in the studio at once and
//! the person's in the agent's log. Or it is an empty in-memory tree ([`Document::empty`]) for tests and for an app
//! that could not open a folder.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::sync::mpsc::Receiver;

use omuzaki_app::{AppCore, AppError, AppOptions};
use omuzaki_core::time::SystemClock;
use omuzaki_doc::{Project, ProjectLock, StoreError};
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, Scratch, Value};
pub use omuzaki_mcp::HttpOptions;
use omuzaki_ops::Executor;
pub use omuzaki_ops::ExecutorError;
use omuzaki_ops_api::{
    Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, Services, Sessions, TxId,
};

/// The name a project folder the app makes for itself is given.
const DEFAULT_PROJECT_NAME: &str = "My character";

/// The document the executor works on, opaque outside this module so nothing else can write to it.
pub struct Document {
    doc: Tree,
    dir: Option<PathBuf>,
}

/// What a [`Document`] holds: an in-memory tree, or a project folder as its files read before the app core opens it.
enum Tree {
    Memory(Scratch),
    Project(Project),
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

    /// The project in `dir`, made there first when the folder holds none. [`core_on`] opens it through the app core,
    /// which journals every applied change.
    ///
    /// # Errors
    /// [`StoreError`] when the folder can't be read or made, or its files are broken, and
    /// [`StoreError::Locked`] when another Omuzaki has it open.
    pub fn project(dir: &Path) -> Result<Self, StoreError> {
        let project = match Project::open(dir) {
            Ok(project) => project,
            Err(StoreError::NotAProject { .. }) => Project::create(dir, DEFAULT_PROJECT_NAME)?,
            Err(StoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Project::create(dir, DEFAULT_PROJECT_NAME)?
            }
            Err(error) => return Err(error),
        };
        // Refused now, while the app can still fall back to an in-memory document, rather than when the core opens.
        drop(ProjectLock::acquire(dir)?);
        Ok(Self { doc: Tree::Project(project), dir: Some(dir.to_path_buf()) })
    }

    /// The project folder, when the document is one.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The value at `path`, read before an executor takes the document (the app reads what it stages first). For a
    /// project it is what the files hold; after a crash, the core's recovery may bring back later work.
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
/// [`CoreError`] when two linked crates register one op name (a bug in whichever registered it).
pub fn runner(services: Services) -> Result<Arc<dyn Runner>, CoreError> {
    runner_on(Document::empty(), services)
}

/// The runner over `document`: every op linked into the app, with `services`. For a project, the runner keeps its
/// app core open; the core closes when the last clone of the runner goes.
///
/// # Errors
/// [`CoreError`] when the ops or the project don't open.
pub fn runner_on(document: Document, services: Services) -> Result<Arc<dyn Runner>, CoreError> {
    core_on(document, services).map(|core| core.runner)
}

/// Why the app's executor did not start.
#[derive(Debug)]
#[non_exhaustive]
pub enum CoreError {
    /// The op registry is broken: two linked crates register one op name (a bug in whichever registered it).
    Ops(ExecutorError),
    /// The app core could not open the project: it is open in another Omuzaki, its files or journal don't load, or a
    /// newer Omuzaki wrote them.
    Project(AppError),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ops(error) => error.fmt(f),
            Self::Project(error) => write!(f, "the project did not open: {error}"),
        }
    }
}

impl std::error::Error for CoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Ops(error) => Some(error),
            Self::Project(error) => Some(error),
        }
    }
}

/// The one executor of the app, seen through the contracts the UI uses: the [`Runner`] every change goes through and
/// the agent [`Sessions`] the Agent panel shows. Both are the same executor, so an agent's session and the ops it runs
/// are one history. Over a project they are the app core's ([`app`](Self::app)), which every MCP client shares.
#[derive(Clone)]
pub struct Core {
    /// Runs every op, for the person and for every agent.
    pub runner: Arc<dyn Runner>,
    /// The agent sessions: who is connected, what they may do, pause and stop.
    pub sessions: Arc<dyn Sessions>,
    app: Option<Arc<AppCore>>,
}

impl fmt::Debug for Core {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Core").field("project", &self.app.as_ref().map(|app| app.dir())).finish_non_exhaustive()
    }
}

impl Core {
    /// The app core of the open project; `None` over an in-memory document.
    pub fn app(&self) -> Option<&Arc<AppCore>> {
        self.app.as_ref()
    }

    /// Opens the project's loopback MCP port with `options`, so other apps and `omuzaki mcp` started in the project
    /// folder work on this same document, and returns its URL. `None`, logged, over an in-memory document or when the
    /// port can't be bound (another studio holds it): agents then reach the studio only from inside it.
    pub fn serve_mcp(&self, options: HttpOptions) -> Option<String> {
        let app = self.app.as_ref()?;
        match app.serve_http(options) {
            Ok(url) => {
                tracing::info!(%url, "agents can connect over MCP");
                Some(url)
            }
            Err(error) => {
                tracing::warn!(%error, "the MCP port did not open: agents can't connect from outside the studio");
                None
            }
        }
    }

    /// Makes the project's files hold every change applied so far, as the studio quits. The journal already holds
    /// them, so a failure here (logged) loses nothing: the next open recovers it.
    pub fn save(&self) {
        if let Some(app) = &self.app
            && let Err(error) = app.save()
        {
            tracing::warn!(%error, "the project's files were not brought up to date; the next open recovers them");
        }
    }
}

/// The app's executor over `document`: every op linked into the app, with `services`. A project opens through the app
/// core as the studio's person, with `services` beside the core's own; see [`Document::project`].
///
/// # Errors
/// [`CoreError`] when the ops or the project don't open.
pub fn core_on(document: Document, services: Services) -> Result<Core, CoreError> {
    match (document.doc, document.dir) {
        (Tree::Project(files), Some(dir)) => {
            // The core reads the folder itself, through its journal: the copy read for staging goes first.
            drop(files);
            let options = AppOptions::studio(Actor::Person { name: PERSON.to_owned() }).with_services(services);
            let app = Arc::new(AppCore::open(&dir, options).map_err(CoreError::Project)?);
            let runner: Arc<dyn Runner> = Arc::new(Holding { runner: app.runner(), _app: Arc::clone(&app) });
            Ok(Core { runner, sessions: app.sessions(), app: Some(app) })
        }
        (Tree::Project(files), None) => in_memory(Box::new(Scratch::of(&files)), services),
        (Tree::Memory(tree), _) => in_memory(Box::new(tree), services),
    }
}

/// The person at the studio, as every op they run is recorded.
const PERSON: &str = "you";

/// An executor over an in-memory `tree`, with `services`.
fn in_memory(tree: Box<Scratch>, services: Services) -> Result<Core, CoreError> {
    let executor = Executor::new(tree, Arc::new(SystemClock::new())).map_err(CoreError::Ops)?;
    let executor = Arc::new(executor.with_services(services));
    Ok(Core { runner: executor.clone(), sessions: executor, app: None })
}

/// The app core's runner, holding the core open while any clone of it is alive: a panel or a script that keeps the
/// runner keeps the project open, and the core closes (its files written, its journal closed) when the last goes.
struct Holding {
    runner: Arc<dyn Runner>,
    _app: Arc<AppCore>,
}

impl Runner for Holding {
    fn ops(&self) -> Vec<OpInfo> {
        self.runner.ops()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.runner.run(req)
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.runner.begin(label, actor)
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.runner.commit(tx, actor)
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.runner.rollback(tx, actor)
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.runner.undo(actor, grants)
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.runner.redo(actor, grants)
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.runner.subscribe(grants)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.runner.get(path, grants)
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.runner.log(after_seq, limit, grants)
    }
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
