//! [`AppCore`]: one open project, its executor, its journal and its MCP server.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use omuzaki_core::time::SystemClock;
use omuzaki_doc::journal::{JournalOptions, ProjectJournal, SessionStart, SystemWall, reopen};
use omuzaki_doc::{Project, ProjectLock};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_history_api::{JournalSink, JournalStatus};
use omuzaki_mcp::{AccessToken, Connections, HttpOptions, HttpServer, InAppLink, InAppSpec, McpOptions, McpServer};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, ConnectionService, Event, Grants, Jobs, Mode, OpError, Outcome, ProjectDir, Request, Runner,
    ServicesBuilder, Sessions,
};
use tokio::runtime::Runtime;

use crate::AppError;
use crate::recover::{self, RecoveryReport};

/// How long closing may take to make everything durable and save the files.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long [`AppCore::save`] waits by default.
const SAVE_TIMEOUT: Duration = Duration::from_secs(10);
/// Worker threads of the MCP runtime: the server is I/O-bound and every call it makes runs on the executor's lock.
const MCP_WORKERS: usize = 2;

/// How to open a project.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AppOptions {
    /// The person working in the studio (or at the command line): the actor of [`AppCore::run`], the person every
    /// MCP session works for.
    pub person: Actor,
    /// Whether a person can be asked to approve an agent's request (the studio's window). Headless hosts say no, so
    /// a request that would wait is refused at once with a hint.
    pub approver: bool,
    /// The grants an MCP client gets unless a token says otherwise.
    pub agent_grants: Grants,
    /// The journal's tunables.
    pub journal: JournalOptions,
    /// The version written into the journal's session records.
    pub app_version: String,
}

impl AppOptions {
    /// The studio's options for `person`: approvals are asked in the window, agents get the default grants.
    pub fn studio(person: Actor) -> Self {
        Self {
            person,
            approver: true,
            agent_grants: Grants::agent_default(),
            journal: JournalOptions::default(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// The options of a host with no window (`omuzaki` at the command line): nobody can approve, so requests that
    /// would wait are refused.
    pub fn headless(person: Actor) -> Self {
        Self { approver: false, ..Self::studio(person) }
    }

    /// These options with the journal's tunables set.
    #[must_use]
    pub fn with_journal(mut self, journal: JournalOptions) -> Self {
        self.journal = journal;
        self
    }

    /// These options with the default grants of MCP clients set.
    #[must_use]
    pub fn with_agent_grants(mut self, grants: Grants) -> Self {
        self.agent_grants = grants;
        self
    }
}

/// One open project: the executor every surface runs ops on, its journal, and the MCP server that serves it. See the
/// [crate docs](crate).
///
/// Every method takes `&self`; share it in an `Arc` between the UI thread and whatever else drives it.
pub struct AppCore {
    executor: Arc<Executor>,
    journal: Arc<ProjectJournal>,
    mcp: McpServer,
    runtime: Runtime,
    http: Mutex<Option<HttpServer>>,
    person: Actor,
    dir: PathBuf,
    recovery: RecoveryReport,
    /// Held for as long as the project is open: one writer per folder.
    lock: Mutex<Option<ProjectLock>>,
}

/// The data even if a thread panicked holding the lock: each critical section leaves it consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AppCore {
    /// Creates a project called `name` in `dir` and opens it.
    ///
    /// # Errors
    /// As [`Project::create`] and [`AppCore::open`].
    pub fn create(dir: impl AsRef<Path>, name: &str, options: AppOptions) -> Result<Self, AppError> {
        Project::create(dir.as_ref(), name)?;
        Self::open(dir, options)
    }

    /// Opens the project in `dir`: takes its lock, recovers it if the last session didn't close, restores its history
    /// and starts a journalled session. See the [crate docs](crate) for the steps.
    ///
    /// # Errors
    /// [`AppError::Store`] if the folder isn't a project, is open in another Omuzaki
    /// ([`StoreError::Locked`](omuzaki_doc::StoreError::Locked)) or doesn't load; [`AppError::Journal`] if the journal
    /// can't be read or started (a newer Omuzaki wrote it); [`AppError::Executor`] if two linked ops share a name.
    pub fn open(dir: impl AsRef<Path>, options: AppOptions) -> Result<Self, AppError> {
        let dir = dir.as_ref();
        let project_dir = ProjectDir::open(dir).map_err(|e| OpError::invalid("project", e.to_string()))?;
        let folder_lock = ProjectLock::acquire(dir)?;
        let reopened = reopen(dir)?;
        let wall = Arc::new(SystemWall);
        let mut shadow = reopened.project;

        let connections = Connections::default();
        let services = ServicesBuilder::new().insert::<dyn ConnectionService>(Arc::new(connections.clone())).build();
        let executor =
            Executor::restore(Box::new(shadow.clone()), Arc::new(SystemClock::new()), wall.clone(), &reopened.history)?
                .with_services(services)
                .with_project_dir(project_dir)
                .with_approver(options.approver);
        let mut recovery = RecoveryReport::default();
        recover::replay(&executor, &reopened.tail, reopened.recovery.map(|r| r.end), &mut recovery);
        recover::close_open(&executor, reopened.last_lsn, &mut recovery);

        // The keeper's copy starts where the executor's does: after the replay and the closed transactions.
        let root = executor.get(&DocPath::root(), &Grants::person())?.unwrap_or_default();
        shadow.reset_to(&root).map_err(|e| OpError::internal(format!("the recovered project can't be stored: {e}")))?;
        let start =
            SessionStart::new(session_id(), &options.app_version, reopened.last_lsn, executor.history_export(), wall);
        let journal = ProjectJournal::start(shadow, start, options.journal)?;
        let executor = Arc::new(executor.with_journal(journal.clone()));

        let root_path = project_dir_text(dir);
        let mcp_options = McpOptions::with_grants(options.agent_grants)
            .with_connections(connections)
            .with_sessions(executor.clone(), options.person.clone())
            .with_journal(journal.clone())
            .with_roots(root_path, std::iter::empty::<String>());
        let mcp = McpServer::new(executor.clone(), mcp_options)?.with_jobs(executor.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(MCP_WORKERS)
            .thread_name("omuzaki-mcp")
            .enable_all()
            .build()
            .map_err(AppError::Runtime)?;
        if !recovery.is_empty() {
            tracing::warn!(recovery = %recovery.summary(), "opened after recovery");
        }
        Ok(Self {
            executor,
            journal,
            mcp,
            runtime,
            http: Mutex::new(None),
            person: options.person,
            dir: dir.to_path_buf(),
            recovery,
            lock: Mutex::new(Some(folder_lock)),
        })
    }

    /// The runner every surface uses: the UI's panels, the command palette, scripts.
    pub fn runner(&self) -> Arc<dyn Runner> {
        self.executor.clone()
    }

    /// The session registry: the Agent panel lists, pauses and revokes agent sessions through it, and resolves their
    /// approvals.
    pub fn sessions(&self) -> Arc<dyn Sessions> {
        self.executor.clone()
    }

    /// The job manager: long ops with progress and cancel.
    pub fn jobs(&self) -> Arc<dyn Jobs> {
        self.executor.clone()
    }

    /// The executor itself, for a host that needs what only it offers (its history export, job polling).
    pub fn executor(&self) -> &Arc<Executor> {
        &self.executor
    }

    /// The MCP server over this core: serve it on any transport (`omuzaki mcp` serves it on stdio).
    pub fn mcp(&self) -> &McpServer {
        &self.mcp
    }

    /// The runtime the MCP server runs on, for a host that serves it on a transport of its own.
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The person this core works for.
    pub fn person(&self) -> &Actor {
        &self.person
    }

    /// The project folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What opening had to recover, empty after a clean close.
    pub fn recovery(&self) -> &RecoveryReport {
        &self.recovery
    }

    /// Whether changes are being saved: for the status banner.
    pub fn journal_status(&self) -> JournalStatus {
        self.journal.status()
    }

    /// Runs `op` with `args` as the person, applied. The UI's panels and the command palette call this (or
    /// [`runner`](Self::runner) directly, for a transaction or a dry run).
    ///
    /// # Errors
    /// As [`Runner::run`].
    pub fn run(&self, op: &str, args: Value) -> Result<Outcome, OpError> {
        self.runner().run(self.request(op, args, Mode::Apply))
    }

    /// Plans `op` with `args` as the person and returns the plan, changing nothing.
    ///
    /// # Errors
    /// As [`Runner::run`].
    pub fn dry_run(&self, op: &str, args: Value) -> Result<Outcome, OpError> {
        self.runner().run(self.request(op, args, Mode::DryRun))
    }

    /// A request by the person, with the person's grants.
    pub fn request(&self, op: &str, args: Value, mode: Mode) -> Request {
        Request {
            op: op.to_owned(),
            args,
            mode,
            actor: self.person.clone(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        }
    }

    /// Undoes the person's last step; returns its label, or `None` when there is nothing to undo.
    ///
    /// # Errors
    /// As [`Runner::undo`].
    pub fn undo(&self) -> Result<Option<String>, OpError> {
        self.executor.undo(&self.person, &Grants::person())
    }

    /// Redoes the person's last undone step.
    ///
    /// # Errors
    /// As [`Runner::redo`].
    pub fn redo(&self) -> Result<Option<String>, OpError> {
        self.executor.redo(&self.person, &Grants::person())
    }

    /// Every event from now on: ops applied by anyone (the person, an agent, a script), commits, undos, sessions,
    /// approvals and jobs, in log order.
    pub fn subscribe(&self) -> Receiver<Event> {
        // The person holds `read`, which is all a subscription needs, so this can't be refused.
        self.executor.subscribe(&Grants::person()).unwrap_or_else(|_| std::sync::mpsc::channel().1)
    }

    /// Reads the document at `path` (the root for everything).
    pub fn get(&self, path: &DocPath) -> Option<Value> {
        self.executor.get(path, &Grants::person()).ok().flatten()
    }

    /// Saves the project files now and returns once they hold every change made before the call.
    ///
    /// # Errors
    /// [`AppError::Sync`] with the reason when they can't be saved (a file changed outside Omuzaki, a full disk).
    pub fn save(&self) -> Result<(), AppError> {
        self.journal.save_now(SAVE_TIMEOUT)?;
        Ok(())
    }

    /// Opens the loopback MCP port (`127.0.0.1:7437/mcp` by default) for other apps. Clients need a token from
    /// [`issue_token`](Self::issue_token). Returns the URL. A second call returns the port already open.
    ///
    /// # Errors
    /// [`AppError::Mcp`] if the port can't be bound.
    pub fn serve_http(&self, options: HttpOptions) -> Result<String, AppError> {
        let mut http = lock(&self.http);
        if let Some(port) = http.as_ref() {
            return Ok(port.url());
        }
        let port = self.runtime.block_on(self.mcp.serve_http(options))?;
        let url = port.url();
        *http = Some(port);
        Ok(url)
    }

    /// Issues an access token for the HTTP port, carrying exactly `grants`, labelled `label` in the Agent panel.
    ///
    /// # Errors
    /// [`AppError::Op`] if the port isn't open; [`AppError::Mcp`] if the token can't be made.
    pub fn issue_token(&self, label: &str, grants: Grants) -> Result<AccessToken, AppError> {
        let http = lock(&self.http);
        let port = http.as_ref().ok_or_else(|| OpError::unavailable("mcp-http").hint("open the port first"))?;
        Ok(port.issue(label, grants)?)
    }

    /// Opens a session for the built-in assistant (or a sub-agent) on an in-memory stream: an MCP client of this same
    /// server, with the same tools and rules as any other.
    ///
    /// # Errors
    /// [`AppError::Mcp`] if the registry refuses the session (a ceiling above the person's).
    pub fn connect_in_app(&self, spec: InAppSpec) -> Result<InAppLink, AppError> {
        Ok(self.runtime.block_on(self.mcp.connect_in_app(spec))?)
    }

    /// Ends the session cleanly: closes the HTTP port (its sessions end), cancels jobs, saves the files and writes the
    /// close snapshot, so the next open restores the document and every undo and redo step without replaying.
    ///
    /// # Errors
    /// [`AppError::Sync`] if saving fails or times out. Nothing applied is lost: the next open recovers it from the
    /// journal.
    pub fn close(self) -> Result<(), AppError> {
        self.shut_down()
    }

    /// Stops without closing, as a crash does: the journal's threads stop, nothing more is saved, and the next open
    /// recovers from the journal. For tests and for a host that must exit at once.
    pub fn abandon(self) {
        self.stop_serving();
        // Without the lock, the drop that follows doesn't close: the journal stops when its last owner goes.
        drop(lock(&self.lock).take());
    }

    fn stop_serving(&self) {
        if let Some(port) = lock(&self.http).take() {
            self.runtime.block_on(port.shutdown());
        }
        self.executor.shutdown_jobs();
    }

    fn shut_down(&self) -> Result<(), AppError> {
        // Closed once: the lock goes with the close, so a second call (the drop after `close`) does nothing.
        let Some(folder_lock) = lock(&self.lock).take() else { return Ok(()) };
        self.stop_serving();
        let closed = self.journal.close(self.executor.history_export(), CLOSE_TIMEOUT);
        drop(folder_lock);
        closed.map(drop).map_err(AppError::from)
    }
}

impl Drop for AppCore {
    fn drop(&mut self) {
        if let Err(error) = self.shut_down() {
            tracing::warn!(%error, "the project didn't close cleanly; the next open recovers it from the journal");
        }
    }
}

/// The project folder as agents are granted it: its real path (ADR 0013), or as given when it can't be resolved.
fn project_dir_text(dir: &Path) -> String {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()).display().to_string()
}

/// A new random session id: 64 random bits, as a number.
fn session_id() -> String {
    let mut bytes = [0u8; 8];
    // Without OS randomness the id only has to differ from the last session's, which the process id and the time do.
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let mixed = nanos ^ u128::from(std::process::id());
        bytes.copy_from_slice(&mixed.to_le_bytes()[..8]);
    }
    u64::from_le_bytes(bytes).to_string()
}
