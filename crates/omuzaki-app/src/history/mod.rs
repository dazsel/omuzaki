//! History as a person or an agent reaches it (VC-01, VC-03; `docs/design/branches.md` §6 and §9, ADR 0021): the
//! transactions of every session, the document at any of them, and the ops that list, compare, restore and name
//! them (`history.list`, `history.diff`, `history.restore`, `history.name`).
//!
//! The state just after a transaction is rebuilt, never stored: the newest snapshot at or before the transaction's
//! commit, restored into a scratch executor, plus the journal's records up to the commit, replayed by the same code
//! recovery uses ([`Executor::replay`]). A session's records always follow its base snapshot, so the replay never
//! crosses a session boundary; one that would (a base snapshot lost) closes what the earlier session left open, as
//! reopening does. The live document is never touched.

mod ops;
pub mod units;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use omuzaki_core::time::SystemClock;
use omuzaki_doc::Project;
use omuzaki_doc::journal::{JournalHistory, Record, SystemWall};
use omuzaki_doc::snapshot::{Facts, SessionSnapshots, Snapshotter};
use omuzaki_doc_api::Patch;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_history_api::branch::{BranchError, BranchId};
use omuzaki_history_api::state::{BranchStore, HistoryError, HistoryView, StateRef};
use omuzaki_history_api::{
    HistoryEntry, HistoryExport, HistoryLog, Lsn, Reload, SnapshotError, SnapshotId, SnapshotInfo, Snapshots, Trigger,
    TxOutcome, Usage, WallClock,
};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Grants, Runner, TxId};

/// How many rebuilt states are kept for the next read.
const CACHE: usize = 8;

/// The tree of any state of the project: [`HistoryView`] over the journal, the archive and the snapshots.
pub struct ProjectHistory {
    snapshots: Arc<Snapshotter>,
    log: Arc<JournalHistory>,
    cache: Mutex<VecDeque<(TxId, Arc<Project>)>>,
    /// The branch whose history this is: its snapshots are the bases its states are rebuilt from.
    branch: BranchId,
    /// Where points and other branches' heads are read, for `{"point": …}` and `{"branch": …}`.
    branches: Option<Arc<dyn BranchStore>>,
}

impl ProjectHistory {
    /// The history of the project whose snapshots `snapshots` keeps and whose journal `log` reads.
    pub fn new(snapshots: Arc<Snapshotter>, log: Arc<JournalHistory>) -> Self {
        Self { snapshots, log, cache: Mutex::new(VecDeque::new()), branch: BranchId::main(), branches: None }
    }

    /// This history, of branch `branch`'s lane, reading points and branches from `branches`.
    #[must_use]
    pub fn on_branch(mut self, branch: BranchId, branches: Arc<dyn BranchStore>) -> Self {
        self.branch = branch;
        self.branches = Some(branches);
        self
    }

    /// The branch whose history this is.
    pub fn branch(&self) -> &BranchId {
        &self.branch
    }

    /// The project's transactions.
    pub fn log(&self) -> &Arc<JournalHistory> {
        &self.log
    }

    /// The project as it was just after transaction `tx` committed, and the LSN of that commit.
    ///
    /// # Errors
    /// [`HistoryError::UnknownTx`] for a transaction the journal does not hold or that never committed (rolled back,
    /// or still open); [`HistoryError::Snapshot`] when no snapshot before it loads; [`HistoryError::Io`] when the
    /// journal can't be read or its records don't replay over the snapshot.
    pub fn project_at(&self, tx: TxId) -> Result<(Arc<Project>, Lsn), HistoryError> {
        let item = self.log.get(tx)?;
        if matches!(item.outcome, TxOutcome::RolledBack | TxOutcome::Open) {
            return Err(HistoryError::UnknownTx(tx));
        }
        let commit = item.last;
        if let Some(hit) = self.cached(tx) {
            return Ok((hit, commit));
        }
        let records = self.log.records()?;
        let base = self
            .snapshots
            .refs()?
            .into_iter()
            .filter(|info| {
                info.branch == self.branch
                    && info.lsn <= commit
                    && !matches!(info.trigger, Trigger::PreRecovery | Trigger::PreMigration | Trigger::Point)
            })
            .find_map(|info| Some((self.snapshots.load(&info.id).ok()?, self.snapshots.history(&info).ok()?, info)));
        let Some((mut project, history, info)) = base else {
            return Err(HistoryError::Snapshot(SnapshotError::Invalid(format!(
                "no snapshot from before transaction {} is kept, so its state can't be rebuilt",
                tx.0
            ))));
        };
        let root = replay(project.clone(), &history, &records, info.lsn, commit)?;
        project.reset_to(&root).map_err(|e| HistoryError::Io(format!("the rebuilt state doesn't store: {e}")))?;
        let project = Arc::new(project);
        self.remember(tx, Arc::clone(&project));
        Ok((project, commit))
    }

    fn cached(&self, tx: TxId) -> Option<Arc<Project>> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.iter().find(|(at, _)| *at == tx).map(|(_, project)| Arc::clone(project))
    }

    fn remember(&self, tx: TxId, project: Arc<Project>) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() == CACHE {
            cache.pop_front();
        }
        cache.push_back((tx, project));
    }
}

/// The document after replaying the records in `(from, to]` over `project` restored with `history`.
fn replay(
    project: Project,
    history: &HistoryExport,
    records: &[(Lsn, Record)],
    from: Lsn,
    to: Lsn,
) -> Result<Value, HistoryError> {
    let io = |e: &dyn std::fmt::Display| HistoryError::Io(e.to_string());
    let wall: Arc<dyn WallClock> = Arc::new(SystemWall);
    let executor =
        Executor::restore(Box::new(project), Arc::new(SystemClock::new()), wall, history).map_err(|e| io(&e))?;
    let mut run: Vec<(Lsn, HistoryEntry)> = Vec::new();
    for (lsn, record) in records.iter().filter(|(lsn, _)| *lsn > from && *lsn <= to) {
        match record {
            Record::Entry(entry) => run.push((*lsn, entry.clone())),
            Record::Session(_) => {
                executor.replay(&std::mem::take(&mut run)).map_err(|e| io(&e))?;
                crate::recover::close_open(&executor, *lsn, &mut crate::RecoveryReport::default());
            }
            _ => {}
        }
    }
    executor.replay(&run).map_err(|e| io(&e))?;
    Ok(executor.get(&DocPath::root(), &Grants::person()).map_err(|e| io(&e))?.unwrap_or_default())
}

impl HistoryView for ProjectHistory {
    fn tree(&self, at: &StateRef) -> Result<Arc<Value>, HistoryError> {
        match at {
            StateRef::Tx { tx, .. } => Ok(Arc::new(self.project_at(*tx)?.0.root().clone())),
            StateRef::Snapshot { id, .. } => Ok(Arc::new(self.snapshots.load(id)?.root().clone())),
            StateRef::Point { id, .. } => match &self.branches {
                Some(branches) => Ok(branches.tree(id)?),
                None => Err(HistoryError::Branch(BranchError::PointNotFound(*id))),
            },
            StateRef::Branch { name, .. } => match &self.branches {
                Some(branches) => {
                    let branch = branches.branch(name)?;
                    if branch.id == self.branch {
                        return Err(HistoryError::Io(
                            "this branch's own state is `now`: read it from the op's document".to_owned(),
                        ));
                    }
                    Ok(branches.tree(&branch.head)?)
                }
                None => Err(HistoryError::Branch(BranchError::NotFound(name.to_string()))),
            },
            _ => Err(HistoryError::Io("`now` is the op's own document: read it there".to_owned())),
        }
    }
}

/// The session's [`Snapshots`] with [`take_at`](Snapshots::take_at) for past transactions: named versions of any
/// state of the history.
pub struct HistorySnapshots {
    inner: Arc<SessionSnapshots>,
    history: Arc<ProjectHistory>,
    snapshots: Arc<Snapshotter>,
    session: String,
    branch: BranchId,
}

impl HistorySnapshots {
    /// `inner`, able to snapshot the past states `history` rebuilds, for the session `session`.
    pub fn new(
        inner: Arc<SessionSnapshots>,
        history: Arc<ProjectHistory>,
        snapshots: Arc<Snapshotter>,
        session: impl Into<String>,
    ) -> Self {
        Self { inner, history, snapshots, session: session.into(), branch: BranchId::main() }
    }

    /// This service, for the lane of `branch`: the versions it takes of past states name the branch.
    #[must_use]
    pub fn on_branch(mut self, branch: BranchId) -> Self {
        self.branch = branch;
        self
    }
}

impl Snapshots for HistorySnapshots {
    fn list(&self) -> Result<Vec<SnapshotInfo>, SnapshotError> {
        self.inner.list()
    }

    fn take(&self, trigger: Trigger, name: Option<&str>) -> Result<SnapshotId, SnapshotError> {
        self.inner.take(trigger, name)
    }

    fn take_at(&self, state: &StateRef, trigger: Trigger, name: Option<&str>) -> Result<SnapshotId, SnapshotError> {
        let tx = match state {
            StateRef::Now => return self.inner.take(trigger, name),
            StateRef::Tx { tx, .. } => *tx,
            other => {
                return Err(SnapshotError::Invalid(format!(
                    "a version can be taken of now or of a transaction, not of {}",
                    serde_json::to_string(other).unwrap_or_default()
                )));
            }
        };
        let (project, lsn) = self.history.project_at(tx).map_err(|e| match e {
            HistoryError::Snapshot(error) => error,
            other => SnapshotError::Invalid(other.to_string()),
        })?;
        let mut facts =
            Facts::new(lsn, self.session.clone(), trigger, SystemWall.now()).with_branch(self.branch.clone());
        facts = match (trigger, name) {
            (Trigger::Named, Some(name)) if !name.trim().is_empty() => facts.with_name(name),
            (Trigger::Take, Some(stem)) => facts.with_stem(stem),
            (Trigger::Take | Trigger::Manual, None) => facts,
            _ => {
                return Err(SnapshotError::Invalid(
                    "a name goes with a named version, a stem with a take, and no other snapshot takes one".to_owned(),
                ));
            }
        };
        Ok(self.snapshots.take_rendered(&project, facts, &HistoryExport::empty())?.info.id)
    }

    fn tree(&self, id: &SnapshotId) -> Result<Value, SnapshotError> {
        self.inner.tree(id)
    }

    fn restore_outside_tree(&self, id: &SnapshotId) -> Result<(), SnapshotError> {
        self.inner.restore_outside_tree(id)
    }

    fn save_files(&self, timeout: Duration) -> Result<(), SnapshotError> {
        self.inner.save_files(timeout)
    }

    fn reload_patch(&self, paths: &[String]) -> Result<Patch, SnapshotError> {
        self.inner.reload_patch(paths)
    }

    fn reload(&self, paths: &[String]) -> Result<Reload, SnapshotError> {
        self.inner.reload(paths)
    }

    fn usage(&self) -> Result<Usage, SnapshotError> {
        self.inner.usage()
    }
}
