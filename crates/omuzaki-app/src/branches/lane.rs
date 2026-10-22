//! One lane (`docs/design/branches.md` 5.1 and 5.2): ADR 0021's stack for one branch. An executor with its sessions,
//! a journal on the branch's root, a keeper that saves the files only for the folder lane, and an MCP server over
//! that executor, so an agent session works on exactly one branch.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Instant;

use omuzaki_core::time::SystemClock;
use omuzaki_doc::branch::{DiskBranchStore, Refs};
use omuzaki_doc::journal::{JournalFs, JournalHistory, JournalOptions, ProjectJournal, Reopened, SessionStart};
use omuzaki_doc::snapshot::{SessionSnapshots, Snapshotter};
use omuzaki_doc_api::DocPath;
use omuzaki_history_api::branch::{BranchId, Branches, LaneState};
use omuzaki_history_api::state::{BranchStore, HistoryView};
use omuzaki_history_api::{HistoryExport, HistoryLog, Snapshots, WallClock};
use omuzaki_mcp::{BranchHost, Connections, LaneServers, McpOptions, McpServer, ToolMode};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, ConnectionService, Grants, ProjectDir, Runner, Services, ServicesBuilder, SessionState};

use crate::AppError;
use crate::core::with_resource_providers;
use crate::history::{HistorySnapshots, ProjectHistory};
use crate::recover::{self, RecoveryReport};

/// The lane state every branch store reads: kept by the lane manager, read by ops under their own lane's lock. It is
/// a leaf lock, never held while another is taken, so an op reading it can't deadlock with the manager.
pub(crate) type LaneStates = Arc<RwLock<BTreeMap<BranchId, LaneState>>>;

/// What every lane of one open project shares.
pub(crate) struct Shared {
    pub dir: PathBuf,
    pub project_dir: ProjectDir,
    pub snapshots: Arc<Snapshotter>,
    pub refs: Arc<Refs>,
    pub person: Actor,
    pub approver: bool,
    pub agent_grants: Grants,
    pub mcp_tools: ToolMode,
    pub journal: JournalOptions,
    pub app_version: String,
    /// The host's services with the asset store added: every lane's executor gets them plus its own.
    pub services: Services,
    pub journal_fs: Option<Arc<dyn JournalFs>>,
    /// The next session id: one counter for every lane, so ids never repeat across branches (design 5.1).
    pub session_ids: Arc<AtomicU64>,
    /// The last approval id handed out: one counter for every lane, so the window resolves an approval by id.
    pub approval_ids: Arc<AtomicU64>,
    pub lane_states: LaneStates,
    pub wall: Arc<dyn WallClock>,
    /// The lane manager as every lane's MCP server reaches it (`branch_create`, `branch_work`).
    pub handle: Arc<super::handle::BranchesHandle>,
}

/// One open lane.
pub struct OpenLane {
    /// Its branch.
    pub branch: BranchId,
    /// Its executor: the runner, the session registry and the job manager of the branch.
    pub executor: Arc<Executor>,
    /// Its journal.
    pub journal: Arc<ProjectJournal>,
    /// The snapshots its ops reach.
    pub snapshots: Arc<SessionSnapshots>,
    /// Its history view: the tree of any state of the branch.
    pub history: Arc<ProjectHistory>,
    /// The MCP server whose sessions work on this branch.
    pub mcp: McpServer,
    /// What opening it had to recover.
    pub recovery: RecoveryReport,
    /// How many owners the executor has with nobody holding a handle: more means a surface holds the lane.
    baseline: usize,
    /// When it last had a session, a handle or the folder.
    last_busy: Mutex<Instant>,
}

impl std::fmt::Debug for OpenLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenLane").field("branch", &self.branch).finish_non_exhaustive()
    }
}

impl OpenLane {
    /// How many sessions work on it now.
    pub fn sessions(&self) -> u32 {
        let live = omuzaki_ops_api::Sessions::sessions(self.executor.as_ref())
            .into_iter()
            .filter(|s| !matches!(s.state, SessionState::Ended { .. }))
            .count();
        u32::try_from(live).unwrap_or(u32::MAX)
    }

    /// Whether a surface holds a handle on it (a `Lane` the Theatre keeps, a runner a host kept).
    pub(crate) fn held(&self) -> bool {
        Arc::strong_count(&self.executor) > self.baseline
    }

    /// Notes that it is busy now, or reports how long it has been idle.
    pub(crate) fn idle_for(&self, busy: bool) -> std::time::Duration {
        let mut last = self.last_busy.lock().unwrap_or_else(PoisonError::into_inner);
        if busy {
            *last = Instant::now();
        }
        last.elapsed()
    }
}

/// The highest session number an agent in `history` carries (`claude-code#12` is 12): new ids start above it.
pub(crate) fn highest_session(history: &HistoryExport) -> u64 {
    let numbered = |actor: &Actor| match actor {
        Actor::Agent { id, .. } => id.rsplit_once('#').and_then(|(_, n)| n.parse::<u64>().ok()),
        _ => None,
    };
    history
        .actors
        .iter()
        .map(|a| &a.actor)
        .chain(history.open.iter().map(|o| &o.owner))
        .filter_map(numbered)
        .max()
        .unwrap_or(0)
}

/// Opens the lane of `branch` over what reopening it found: the executor restored and the tail replayed, open
/// transactions closed, the journal started on the branch's root (saving the files only for the folder lane), and an
/// MCP server over it. `fresh_jobs` removes leftover job scratch folders (the folder lane at project open).
pub(crate) fn open(
    shared: &Shared,
    branch: &BranchId,
    reopened: Reopened,
    folder: bool,
    fresh_jobs: bool,
) -> Result<OpenLane, AppError> {
    let mut shadow = reopened.project;
    // Ids continue above any the branch's history knows, so a restart never hands out one again.
    shared.session_ids.fetch_max(highest_session(&reopened.history) + 1, Ordering::SeqCst);

    let connections = Connections::default();
    let session = crate::core::session_id();
    let session_snapshots = Arc::new(SessionSnapshots::new(shared.snapshots.clone()));
    let log = Arc::new(JournalHistory::on_branch(&shared.dir, branch.clone(), shared.snapshots.clone()));
    let states = Arc::clone(&shared.lane_states);
    let store = Arc::new(
        DiskBranchStore::new(branch.clone(), shared.refs.clone(), shared.snapshots.clone()).with_lanes(move |id| {
            states.read().unwrap_or_else(PoisonError::into_inner).get(id).copied().unwrap_or(LaneState::Closed)
        }),
    );
    let history = Arc::new(
        ProjectHistory::new(shared.snapshots.clone(), log.clone())
            .on_branch(branch.clone(), store.clone() as Arc<dyn BranchStore>),
    );
    let history_snapshots =
        HistorySnapshots::new(session_snapshots.clone(), history.clone(), shared.snapshots.clone(), session.clone())
            .on_branch(branch.clone());
    let services = ServicesBuilder::from(shared.services.clone())
        .insert::<dyn ConnectionService>(Arc::new(connections.clone()))
        .insert::<dyn Snapshots>(Arc::new(history_snapshots))
        .insert::<dyn HistoryLog>(log.clone())
        .insert::<dyn HistoryView>(history.clone())
        .insert::<dyn BranchStore>(store)
        .build();
    let executor = Executor::restore(
        Box::new(shadow.clone()),
        Arc::new(SystemClock::new()),
        shared.wall.clone(),
        &reopened.history,
    )?
    .with_services(services.clone())
    .with_project_dir(shared.project_dir.clone())
    .with_approver(shared.approver)
    .with_last_job_id(reopened.last_job)
    .with_session_counter(Arc::clone(&shared.session_ids))
    .with_approval_counter(Arc::clone(&shared.approval_ids));
    let mut recovery = RecoveryReport::default();
    if let Some(found) = &reopened.recovery {
        recovery.archived = folder.then_some(found.kept);
        recovery.from_disk = found.mode == omuzaki_doc::journal::Recover::Disk;
    }
    recover::replay(&executor, &reopened.tail, reopened.recovery.map(|r| r.end), &mut recovery);
    recover::close_open(&executor, reopened.last_lsn, &mut recovery);

    // The keeper's copy starts where the executor's does: after the replay and the closed transactions.
    let root = executor.get(&DocPath::root(), &Grants::person())?.unwrap_or_default();
    shadow
        .reset_to(&root)
        .map_err(|e| omuzaki_ops_api::OpError::internal(format!("the recovered project can't be stored: {e}")))?;
    let start = SessionStart::new(
        session,
        &shared.app_version,
        reopened.last_lsn,
        executor.history_export(),
        shared.wall.clone(),
        shared.snapshots.clone(),
    )
    .with_last_job(reopened.last_job)
    .on_branch(branch.clone())
    .saving_files(folder);
    let start = match &shared.journal_fs {
        Some(fs) => start.with_fs(Arc::clone(fs)),
        None => start,
    };
    if fresh_jobs {
        recover::remove_job_scratch(&shared.dir);
    }
    let journal = ProjectJournal::start(shadow, start, shared.journal)?;
    recover::end_interrupted(&journal, &reopened.interrupted, &mut recovery);
    session_snapshots.bind(journal.clone());
    log.bind(journal.clone());
    let executor = Arc::new(executor.with_journal(journal.clone()));

    let mcp_options = McpOptions::with_grants(shared.agent_grants.clone())
        .with_tools(shared.mcp_tools)
        .with_connections(connections)
        .with_sessions(executor.clone(), shared.person.clone())
        .with_journal(journal.clone())
        .with_roots(crate::core::project_dir_text(&shared.dir), std::iter::empty::<String>())
        .with_branches(BranchHost::new(
            Arc::clone(&shared.handle) as Arc<dyn Branches>,
            Arc::clone(&shared.handle) as Arc<dyn LaneServers>,
            branch.clone(),
        ));
    let mcp_options = with_resource_providers(mcp_options, &services);
    let mcp = McpServer::new(executor.clone() as Arc<dyn Runner>, mcp_options)?.with_jobs(executor.clone());
    let baseline = Arc::strong_count(&executor);
    Ok(OpenLane {
        branch: branch.clone(),
        executor,
        journal,
        snapshots: session_snapshots,
        history,
        mcp,
        recovery,
        baseline,
        last_busy: Mutex::new(Instant::now()),
    })
}
