//! [`AppCore`]: one open project, its executor, its journal and its MCP server.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_doc::branch::Refs;
use omuzaki_doc::journal::{JournalFs, JournalOptions, ProjectJournal, Recover, SystemWall};
use omuzaki_doc::snapshot::{Snapshotter, snapshot_dir};
use omuzaki_doc::{Project, ProjectLock};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_history_api::branch::{BranchError, BranchId, Branches, MAX_OPEN_LANES};
use omuzaki_history_api::{JournalSink, JournalStatus, Snapshots};
use omuzaki_library::HostLibrary;
use omuzaki_library_api::Library;
use omuzaki_mcp::{AccessToken, HttpOptions, HttpServer, InAppLink, InAppSpec, McpOptions, McpServer, ToolMode};
use omuzaki_mcp::{LibraryProvider, MeasureProvider, MetricsProvider, MetricsSource, ReaderSource, RenderProvider};
use omuzaki_metrics_api::MetricsReader;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, Event, Grants, Jobs, Mode, OpError, Outcome, ProjectDir, Request, Runner, Services, ServicesBuilder,
    Sessions,
};
use omuzaki_settings_api::service::Settings;
use omuzaki_store::{DiskStore, LazyDiskStore};
use omuzaki_store_api::Store;
use tokio::runtime::{Handle, Runtime};

use crate::AppError;
use crate::branches::ids::IdMark;
use crate::branches::routed::FolderRunner;
use crate::branches::{Failpoint, HostBranches, IDLE_CLOSE, LaneStates, Shared};
use crate::recover::RecoveryReport;
use crate::studio;

/// How often idle side lanes are looked for.
const REAP_EVERY: Duration = Duration::from_secs(5);
/// How long [`AppCore::save`] waits by default.
const SAVE_TIMEOUT: Duration = Duration::from_secs(10);
/// The asset store's folder inside the person's data folder (the studio's connector host uses the same).
const ASSET_STORE: &str = "store";
/// The person's own library items inside the data folder (`items/<publisher>/<name>/<version>.toml`).
const LIBRARY: &str = "library";
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
    /// The budget an MCP client's session starts with (default: no money; design `agent-sessions.md` 8.1).
    pub agent_budget: Budget,
    /// Which tools the MCP server lists (one per op, or the core with the op finder).
    pub mcp_tools: ToolMode,
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
    /// The person's local data folder: the asset store lives in its `store/` (beside the keychain's lock file, as the
    /// studio keeps it), offered to every op as `dyn Store` unless the host's services carry one. `None` offers none.
    pub data_dir: Option<PathBuf>,
    /// Whether opening is recorded in the person's session through the host's `dyn Settings` (settings.md D9): the
    /// crash-loop guard is written before the load and cleared after, and the project goes to the top of Open Recent.
    /// The studio's, never a headless host's: the CLI and MCP never restore a session, so they never write one.
    pub remember: bool,
    /// A hook at each step of a branch switch, for crash tests ([`with_switch_failpoint`](Self::with_switch_failpoint)).
    pub failpoint: FailpointChoice,
    /// How many lanes may be open at once (design 5.2): 8 unless a test sets fewer.
    pub max_lanes: usize,
}

/// The hook a core calls at each step of a branch switch (see [`AppOptions::with_switch_failpoint`]).
#[derive(Clone, Default)]
pub struct FailpointChoice(Option<Failpoint>);

impl std::fmt::Debug for FailpointChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "FailpointChoice(set)" } else { "FailpointChoice(none)" })
    }
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
            agent_budget: Budget::default(),
            mcp_tools: ToolMode::All,
            journal: JournalOptions::default(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            services: Services::none(),
            recover: Recover::Restore,
            journal_fs: JournalFsChoice::default(),
            data_dir: None,
            remember: true,
            failpoint: FailpointChoice::default(),
            max_lanes: MAX_OPEN_LANES,
        }
    }

    /// The options of a host with no window (`omuzaki` at the command line): nobody can approve, so requests that
    /// would wait are refused.
    pub fn headless(person: Actor) -> Self {
        Self { approver: false, remember: false, ..Self::studio(person) }
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

    /// These options with the budget MCP clients' sessions start with: what `omuzaki mcp --spend-cap` or a headless
    /// credential sets.
    #[must_use]
    pub fn with_agent_budget(mut self, budget: Budget) -> Self {
        self.agent_budget = budget;
        self
    }

    /// These options with the MCP server listing the tools `mode` says.
    #[must_use]
    pub fn with_mcp_tools(mut self, mode: ToolMode) -> Self {
        self.mcp_tools = mode;
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

    /// These options with `hook` called at each step of a branch switch (`switch:marked`, `switch:old-point`,
    /// `switch:rewritten`): an error stops the switch there, as a crash would. For crash tests.
    #[must_use]
    pub fn with_switch_failpoint(
        mut self,
        hook: impl Fn(&'static str) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.failpoint = FailpointChoice(Some(Arc::new(hook)));
        self
    }

    /// These options with at most `lanes` lanes open at once (at least 1).
    #[must_use]
    pub fn with_max_lanes(mut self, lanes: usize) -> Self {
        self.max_lanes = lanes.max(1);
        self
    }

    /// These options with the person's local data folder, whose `store/` is the asset store (opened on first use).
    #[must_use]
    pub fn with_data_dir(mut self, data: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(data.into());
        self
    }
}

/// One open project: the executor every surface runs ops on, its journal, and the MCP server that serves it. See the
/// [crate docs](crate).
///
/// Every method takes `&self`; share it in an `Arc` between the UI thread and whatever else drives it.
pub struct AppCore {
    /// The lanes: one per open branch, the folder's always.
    branches: Arc<HostBranches>,
    /// The person's runner, session registry and job manager, routed to the lane each call is about.
    routed: Arc<FolderRunner>,
    /// Closes idle side lanes and keeps the session id mark ahead; stopped at close.
    reaper: Mutex<Option<Reaper>>,
    /// The session id mark, written exactly at a clean close.
    ids: Arc<IdMark>,
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

/// `base` plus the MCP resource families the executor's services can serve, in this order: renders (`omuzaki://render/`,
/// from the asset store), measures (always: they compute through the runner), live metrics (always: from the
/// `MetricsReader` or `MetricsSource` the host gave, or else "no studio is running"), the library (from a `Library`). A family whose source is missing is not registered, so it is not
/// listed or templated and its URIs read as unknown resources.
///
/// The set is fixed when the server starts (`McpServer::new` consumes the options), so the server never advertises
/// `resources.listChanged`.
#[must_use]
pub fn with_resource_providers(base: McpOptions, services: &Services) -> McpOptions {
    let mut options = base;
    if let Some(store) = services.get_arc::<dyn Store>() {
        options = options.with_provider(Arc::new(RenderProvider::new(store)));
    }
    options = options.with_provider(Arc::new(MeasureProvider));
    // A studio gives its live numbers as the hub reader its subsystems publish into, or as a source of its own; a
    // host with neither (headless `omuzaki mcp`) answers that no studio is running.
    let metrics = match (services.get_arc::<dyn MetricsSource>(), services.get_arc::<MetricsReader>()) {
        (Some(source), _) => MetricsProvider::studio(source),
        (None, Some(reader)) => MetricsProvider::studio(Arc::new(ReaderSource(MetricsReader::clone(&reader)))),
        (None, None) => MetricsProvider::headless(),
    };
    options = options.with_provider(Arc::new(metrics));
    if let Some(library) = services.get_arc::<dyn Library>() {
        options = options.with_provider(Arc::new(LibraryProvider::new(library)));
    }
    options
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
        // The crash-loop guard is durable before anything is loaded: if the load kills the process, the next start
        // shows the Welcome screen with a notice instead of trying again (D9).
        let remember = options.services.get_arc::<dyn Settings>().filter(|_| options.remember);
        if let Some(settings) = &remember {
            settings.session().begin_open(dir).map_err(AppError::Session)?;
        }
        let store =
            DiskStore::open(snapshot_dir(dir)).map_err(|e| OpError::unavailable("snapshots").hint(e.to_string()))?;
        let snapshots = Arc::new(Snapshotter::new(dir, Arc::new(store)));
        let mut services = ServicesBuilder::from(options.services.clone());
        if let (Some(data), None) = (&options.data_dir, options.services.get::<dyn Store>()) {
            services = services.insert::<dyn Store>(Arc::new(LazyDiskStore::new(data.join(ASSET_STORE))));
        }
        // The library: the first-party items linked into this build and the person's own (design D11). A host that
        // gives its own `Library` keeps it, and then installing is unavailable.
        if options.services.get::<dyn Library>().is_none() {
            let folder = options.data_dir.as_ref().map(|data| data.join(LIBRARY));
            let library = Arc::new(HostLibrary::open(folder.as_deref()));
            services = services.insert::<dyn Library>(Arc::clone(&library) as Arc<dyn Library>).insert(library);
        }
        let session_ids = Arc::new(AtomicU64::new(1));
        let ids = Arc::new(IdMark::open(dir, Arc::clone(&session_ids)));
        let shared = Shared {
            dir: dir.to_path_buf(),
            project_dir,
            snapshots,
            refs: Arc::new(Refs::new(dir)),
            person: options.person.clone(),
            approver: options.approver,
            agent_grants: options.agent_grants.clone(),
            agent_budget: options.agent_budget,
            mcp_tools: options.mcp_tools,
            journal: options.journal,
            app_version: options.app_version.clone(),
            services: services.build(),
            journal_fs: options.journal_fs.0.clone(),
            session_ids,
            approval_ids: Arc::new(AtomicU64::new(0)),
            lane_states: LaneStates::default(),
            wall: Arc::new(SystemWall),
            handle: Arc::default(),
        };
        let failpoint = options.failpoint.0.clone().unwrap_or_else(|| Arc::new(|_| Ok(())));
        let branches = HostBranches::open(shared, options.recover, failpoint, options.max_lanes)?;
        let folder = branches.folder_lane();
        let recovery = folder.recovery.clone();
        let root = folder.executor.get(&DocPath::root(), &Grants::person())?.unwrap_or_default();
        drop(folder);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(MCP_WORKERS)
            .thread_name("omuzaki-mcp")
            .enable_all()
            .build()
            .map_err(AppError::Runtime)?;
        if !recovery.is_empty() {
            tracing::warn!(recovery = %recovery.summary(), "opened after recovery");
        }
        for (_, notice) in branches.recovered() {
            tracing::warn!(%notice, "a branch was recovered");
        }
        if let Some(settings) = &remember {
            record_opened(settings.as_ref(), dir, &root);
        }
        ids.keep_ahead();
        let reaper = Reaper::start(&branches, Arc::clone(&ids));
        Ok(Self {
            ids,
            routed: Arc::new(FolderRunner::new(branches.clone())),
            branches,
            reaper: Mutex::new(Some(reaper)),
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
        self.routed.clone()
    }

    /// The session registry: the Agent panel lists, pauses and revokes agent sessions through it, and resolves their
    /// approvals.
    pub fn sessions(&self) -> Arc<dyn Sessions> {
        self.routed.clone()
    }

    /// The project's snapshots and files: named versions, backups, saving now (the service ops reach them through).
    pub fn snapshots(&self) -> Arc<dyn Snapshots> {
        self.branches.folder_lane().snapshots.clone()
    }

    /// The job manager: long ops with progress and cancel.
    pub fn jobs(&self) -> Arc<dyn Jobs> {
        self.routed.clone()
    }

    /// The folder lane's executor, for a host that needs what only it offers (its history export, job polling). A
    /// switch replaces the folder lane: hold [`runner`](Self::runner) to follow it.
    pub fn executor(&self) -> Arc<Executor> {
        self.branches.folder_lane().executor.clone()
    }

    /// The project's branches (`docs/design/branches.md`): list, create, checkpoint, switch the folder, review,
    /// restore keeping the abandoned future, and each branch's lane.
    pub fn branches(&self) -> &Arc<HostBranches> {
        &self.branches
    }

    /// The MCP server over the folder lane: serve it on any transport (`omuzaki mcp` serves it on stdio). Its sessions
    /// work on the branch that was the folder's when they opened.
    pub fn mcp(&self) -> McpServer {
        self.branches.folder_lane().mcp.clone()
    }

    /// The MCP server over `branch`'s lane, opening the lane if it is closed: `omuzaki mcp --branch` serves it, so an
    /// agent works on its own branch from its first call.
    ///
    /// # Errors
    /// As [`Branches::lane`](omuzaki_history_api::branch::Branches::lane).
    pub fn mcp_on(&self, branch: &BranchId) -> Result<McpServer, AppError> {
        omuzaki_history_api::branch::Branches::lane(self.branches.as_ref(), branch)?;
        let lane = self.branches.open_lane(branch).ok_or_else(|| BranchError::NotFound(branch.to_string()))?;
        Ok(lane.mcp.clone())
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
    pub fn journal(&self) -> Arc<ProjectJournal> {
        self.branches.folder_lane().journal.clone()
    }

    /// Whether changes are being saved: for the status banner.
    pub fn journal_status(&self) -> JournalStatus {
        self.journal().status()
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
        self.routed.undo(&self.person, &Grants::person())
    }

    /// Redoes the person's last undone step.
    ///
    /// # Errors
    /// As [`Runner::redo`].
    pub fn redo(&self) -> Result<Option<String>, OpError> {
        self.routed.redo(&self.person, &Grants::person())
    }

    /// Every event from now on: ops applied by anyone (the person, an agent, a script), commits, undos, sessions,
    /// approvals and jobs, in log order.
    pub fn subscribe(&self) -> Receiver<Event> {
        // The person holds `read`, which is all a subscription needs, so this can't be refused.
        self.routed.subscribe(&Grants::person()).unwrap_or_else(|_| std::sync::mpsc::channel().1)
    }

    /// Reads the document at `path` (the root for everything).
    pub fn get(&self, path: &DocPath) -> Option<Value> {
        self.routed.get(path, &Grants::person()).ok().flatten()
    }

    /// Saves the project files now and returns once they hold every change made before the call.
    ///
    /// # Errors
    /// [`AppError::Sync`] with the reason when they can't be saved (a file changed outside Omuzaki, a full disk).
    pub fn save(&self) -> Result<(), AppError> {
        self.journal().save_now(SAVE_TIMEOUT)?;
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
        let mcp = self.mcp();
        let port = self.runtime().block_on(mcp.serve_http(options))?;
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
        let mcp = self.mcp();
        Ok(self.runtime().block_on(mcp.connect_in_app(spec))?)
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
        // A switch's last steps run to where they stop (a test's failpoint cuts one there), so what is left is what
        // a crash at that step leaves.
        drop(self.branches.settled());
        // Without the lock, the drop that follows doesn't close: the journal stops when its last owner goes.
        drop(lock(&self.lock).take());
    }

    fn stop_serving(&self) {
        if let Some(port) = lock(&self.http).take() {
            studio::withdraw(&self.dir);
            self.runtime().block_on(port.shutdown());
        }
        if let Some(reaper) = lock(&self.reaper).take() {
            reaper.stop();
        }
        for lane in self.branches.lanes() {
            lane.executor.shutdown_jobs();
        }
    }

    fn shut_down(&self) -> Result<(), AppError> {
        // Closed once: the lock goes with the close, so a second call (the drop after `close`) does nothing.
        let Some(folder_lock) = lock(&self.lock).take() else { return Ok(()) };
        self.stop_serving();
        let closed = self.branches.close_all();
        self.ids.close();
        drop(folder_lock);
        closed
    }
}

/// Closes idle side lanes every [`REAP_EVERY`] (design 5.2) and keeps the session id mark ahead, until stopped or the
/// lanes are gone.
struct Reaper {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Reaper {
    fn start(branches: &Arc<HostBranches>, ids: Arc<IdMark>) -> Self {
        let (stop, stopped) = channel();
        let lanes = Arc::downgrade(branches);
        let thread = std::thread::Builder::new()
            .name("omuzaki-lane-reaper".to_owned())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(REAP_EVERY) {
                    let Some(branches) = lanes.upgrade() else { return };
                    branches.close_idle(IDLE_CLOSE);
                    ids.keep_ahead();
                }
            })
            .inspect_err(|error| tracing::warn!(%error, "idle branches stay open until the project closes"))
            .ok();
        Self { stop, thread }
    }

    fn stop(mut self) {
        self.stop.send(()).ok();
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
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

/// Clears the session's crash-loop guard and puts the project at the top of Open Recent. The project is open all the
/// same when that can't be written: the stale guard costs one Welcome screen at the next start.
fn record_opened(settings: &dyn Settings, dir: &Path, root: &Value) {
    let name = root.pointer("/project/name").and_then(Value::as_str).unwrap_or("Untitled");
    if let Err(error) = settings.session().opened(dir, name) {
        tracing::warn!(%error, "the session didn't record the open project");
    }
}

/// The project folder as agents are granted it: its real path (ADR 0013), or as given when it can't be resolved.
#[allow(clippy::disallowed_methods, reason = "the host resolving the folder it was given to open; not an op")]
pub(crate) fn project_dir_text(dir: &Path) -> String {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()).display().to_string()
}

/// A new random session id: 64 random bits, as a number.
pub(crate) fn session_id() -> String {
    let mut bytes = [0u8; 8];
    // Without OS randomness the id only has to differ from the last session's, which the process id and the time do.
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let mixed = nanos ^ u128::from(std::process::id());
        bytes.copy_from_slice(&mixed.to_le_bytes()[..8]);
    }
    u64::from_le_bytes(bytes).to_string()
}
