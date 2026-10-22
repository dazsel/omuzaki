//! [`AppCore`]: one open project, its executor, its journal and its MCP server.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use omuzaki_core::time::SystemClock;
use omuzaki_doc::journal::{JournalFs, JournalOptions, ProjectJournal, Recover, SessionStart, SystemWall, reopen_with};
use omuzaki_doc::snapshot::{SessionSnapshots, Snapshotter, snapshot_dir};
use omuzaki_doc::{Project, ProjectLock};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_history_api::{JournalSink, JournalStatus, Snapshots};
use omuzaki_mcp::{AccessToken, Connections, HttpOptions, HttpServer, InAppLink, InAppSpec, McpOptions, McpServer};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, ConnectionService, Event, Grants, Jobs, Mode, OpError, Outcome, ProjectDir, Request, Runner, Services,
    ServicesBuilder, Sessions,
};
use omuzaki_store::DiskStore;
use tokio::runtime::{Handle, Runtime};

use crate::AppError;
use crate::recover::{self, RecoveryReport};
use crate::studio;

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
    /// The host's own services (the studio's performer, camera input and windows), which the core's executor carries
    /// alongside its own connections.
    pub services: Services,
    /// What opening after a crash does with the files: restore (the default) or open them as they are.
    pub recover: Recover,
    /// The journal writer's file operations: the real file system unless a harness sets its own.
    pub journal_fs: JournalFsChoice,
}

/// The file operations a core's journal writer makes (see [`AppOptions::with_journal_fs`]).
#[derive(Clone, Default)]
pub struct JournalFsChoice(Option<Arc<dyn JournalFs>>);

impl std::fmt::Debug for JournalFsChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "JournalFsChoice(custom)" } else { "JournalFsChoice(std)" })
    }
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
            services: Services::none(),
            recover: Recover::Restore,
            journal_fs: JournalFsChoice::default(),
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

    /// These options with what opening after a crash does with the files (`--recover disk` opens them as they are).
    #[must_use]
    pub fn with_recover(mut self, recover: Recover) -> Self {
        self.recover = recover;
        self
    }

    /// These options with the journal writer's file operations going through `fs`: the power-loss harness simulates a
    /// power cut under it.
    #[must_use]
    pub fn with_journal_fs(mut self, fs: Arc<dyn JournalFs>) -> Self {
        self.journal_fs = JournalFsChoice(Some(fs));
        self
    }

    /// These options with the host's services: the executor gets them plus the core's own (a host's
    /// [`ConnectionService`] is replaced by the core's, which tracks the agents it serves).
    #[must_use]
    pub fn with_services(mut self, services: Services) -> Self {
        self.services = services;
        self
    }
}

/// One open project: the executor every surface runs ops on, its journal, and the MCP server that serves it. See the
/// [crate docs](crate).
///
/// Every method takes `&self`; share it in an `Arc` between the UI thread and whatever else drives it.
pub struct AppCore {
    executor: Arc<Executor>,
    snapshots: Arc<SessionSnapshots>,
    journal: Arc<ProjectJournal>,
    mcp: McpServer,
    /// Always `Some` until the drop, which shuts it down without waiting: a stdio server leaves a thread blocked on
    /// standard input, and a plain drop of the runtime would wait for it forever.
    runtime: Option<Runtime>,
    /// The runtime's handle, which every call uses.
    handle: Handle,
    http: Mutex<Option<HttpServer>>,
    person: Actor,
    /// What a bridged client (`omuzaki mcp` while this core has the project) may do once paired.
    agent_grants: Grants,
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
        let store =
            DiskStore::open(snapshot_dir(dir)).map_err(|e| OpError::unavailable("snapshots").hint(e.to_string()))?;
        let snapshots = Arc::new(Snapshotter::new(dir, Arc::new(store)));
        let reopened = reopen_with(dir, &snapshots, options.recover)?;
        let wall = Arc::new(SystemWall);
        let mut shadow = reopened.project;

        let connections = Connections::default();
        let session_snapshots = Arc::new(SessionSnapshots::new(snapshots.clone()));
        let services = ServicesBuilder::from(options.services.clone())
            .insert::<dyn ConnectionService>(Arc::new(connections.clone()))
            .insert::<dyn Snapshots>(session_snapshots.clone())
            .build();
        let executor =
            Executor::restore(Box::new(shadow.clone()), Arc::new(SystemClock::new()), wall.clone(), &reopened.history)?
                .with_services(services)
                .with_project_dir(project_dir)
                .with_approver(options.approver)
                .with_last_job_id(reopened.last_job);
        let mut recovery = RecoveryReport::default();
        if let Some(found) = &reopened.recovery {
            recovery.archived = Some(found.kept);
            recovery.from_disk = found.mode == Recover::Disk;
        }
        recover::replay(&executor, &reopened.tail, reopened.recovery.map(|r| r.end), &mut recovery);
        recover::close_open(&executor, reopened.last_lsn, &mut recovery);

        // The keeper's copy starts where the executor's does: after the replay and the closed transactions.
        let root = executor.get(&DocPath::root(), &Grants::person())?.unwrap_or_default();
        shadow.reset_to(&root).map_err(|e| OpError::internal(format!("the recovered project can't be stored: {e}")))?;
        let start = SessionStart::new(
            session_id(),
            &options.app_version,
            reopened.last_lsn,
            executor.history_export(),
            wall,
            snapshots,
        )
        .with_last_job(reopened.last_job);
        let start = match options.journal_fs.0 {
            Some(fs) => start.with_fs(fs),
            None => start,
        };
        recover::remove_job_scratch(dir);
        let journal = ProjectJournal::start(shadow, start, options.journal)?;
        recover::end_interrupted(&journal, &reopened.interrupted, &mut recovery);
        session_snapshots.bind(journal.clone());
        let executor = Arc::new(executor.with_journal(journal.clone()));

        let root_path = project_dir_text(dir);
        let mcp_options = McpOptions::with_grants(options.agent_grants.clone())
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
            snapshots: session_snapshots,
            journal,
            mcp,
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
            http: Mutex::new(None),
            person: options.person,
            agent_grants: options.agent_grants,
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

    /// The project's snapshots and files: named versions, backups, saving now (the service ops reach them through).
    pub fn snapshots(&self) -> Arc<dyn Snapshots> {
        self.snapshots.clone()
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

    /// The runtime the MCP server runs on, for a host that serves it on a transport of its own (`block_on` from a
    /// thread outside the runtime).
    pub fn runtime(&self) -> &Handle {
        &self.handle
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

    /// The project's journal: what every change is written to before the files follow. `sync` on it returns once
    /// everything applied before the call is durable.
    pub fn journal(&self) -> &Arc<ProjectJournal> {
        &self.journal
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
    /// It also takes agents started as `omuzaki mcp` (`agent-sessions.md` 10.2): the core publishes the port and a
    /// pairing key in the project's private folder ([`studio`](crate::studio)), a bridge buys its own credential with
    /// the key, and its first session waits on a pairing card until the person pairs it. Paired credentials are stored
    /// beside the key and admitted again here, so a paired client needs no click after a restart; a revoked one is
    /// dropped. The `on_paired` and `on_revoked` hooks of `options` are the core's.
    ///
    /// # Errors
    /// [`AppError::Mcp`] if the port can't be bound.
    pub fn serve_http(&self, options: HttpOptions) -> Result<String, AppError> {
        let mut http = lock(&self.http);
        if let Some(port) = http.as_ref() {
            return Ok(port.url());
        }
        let (paired, revoked) = (self.dir.clone(), self.dir.clone());
        let options = options
            .on_paired(move |record| studio::remember_paired(&paired, record))
            .on_revoked(move |tombstone| studio::forget(&revoked, &tombstone));
        let port = self.runtime().block_on(self.mcp.serve_http(options))?;
        for record in studio::paired_records(&self.dir) {
            let label = record.label.clone();
            if let Err(error) = port.admit(record) {
                tracing::warn!(%error, client = %label, "a paired bridge credential can't be admitted; it pairs again");
            }
        }
        let key = port.enable_pairing(self.agent_grants.clone(), self.person.clone())?;
        let rendezvous = studio::Rendezvous::new(port.url(), port.pair_url(), key);
        if let Err(error) = studio::publish(&self.dir, &rendezvous) {
            tracing::warn!(%error, "couldn't publish the MCP port; `omuzaki mcp` can't bridge into this studio");
        }
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
        Ok(self.runtime().block_on(self.mcp.connect_in_app(spec))?)
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
            studio::withdraw(&self.dir);
            self.runtime().block_on(port.shutdown());
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
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// The project folder as agents are granted it: its real path (ADR 0013), or as given when it can't be resolved.
#[allow(clippy::disallowed_methods, reason = "the host resolving the folder it was given to open; not an op")]
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
