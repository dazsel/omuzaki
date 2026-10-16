//! In-memory branches for op and UI tests (VC-02): [`MemoryBranchStore`] and [`MemoryHistoryView`] are the services
//! ops read, and [`FakeBranches`] is a lane manager that creates, checkpoints, switches, reviews and announces
//! branches without any lanes, journals or disks. Documents are plain values the test sets.
//!
//! The fake follows the contract other crates test against, not a simplification of it: points include what the
//! document's `merges` entity records, forks and restores wait for an open transaction, a lane with sessions stays
//! open when the folder leaves it, and every method that takes an actor enforces the table of design 8.2.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use omuzaki_doc_api::Value;
use omuzaki_history_api::branch::{
    BranchError, BranchEvent, BranchId, BranchInfo, BranchName, Branches, Contains, Lane, LaneState, MAX_OPEN_LANES,
    NewBranch, Point, PointId, PointKind, Restored, Review, ReviewDecision, ReviewState, Switched, Version,
};
use omuzaki_history_api::state::{BranchStore, HistoryError, HistoryView, StateRef};
use omuzaki_history_api::{Lsn, SnapshotError, SnapshotId, WallClock, WallTime};
use omuzaki_ops_api::{Actor, Grants, Runner, Scope, SessionId, SessionState, Sessions, TxId};
use omuzaki_store_api::ContentHash;

use crate::lock;

/// The snapshot id of `tree`: the hash of its JSON, so equal documents share an id, as with a real manifest.
fn snapshot_of(tree: &Value) -> Result<SnapshotId, BranchError> {
    let bytes = serde_json::to_vec(tree).map_err(|e| BranchError::Io(e.to_string()))?;
    Ok(SnapshotId(ContentHash::of(&bytes)))
}

/// The points of other branches `tree` records as merged in (design 7.3): the document's `merges` entity holds a
/// `format` and, for each branch merged in, a table named by the branch id with its `name`, `seq` and `point`.
/// An entry that does not read (an unknown shape, an id this project does not know) is skipped, as the design says of
/// ids that are unknown on another machine; `own` never counts itself.
fn merges_of(tree: &Value, own: &BranchId) -> Contains {
    let Some(entries) = tree.get("merges").and_then(Value::as_object) else { return Contains::new() };
    entries.iter().fold(Contains::new(), |merged, (key, entry)| {
        let branch = key.parse::<BranchId>().ok().filter(|b| b != own);
        let seq = entry.get("seq").and_then(Value::as_u64);
        let point = entry.get("point").and_then(Value::as_str).and_then(|p| p.parse::<PointId>().ok());
        match (branch, seq, point) {
            (Some(branch), Some(seq), Some(point)) => merged.with(branch, Version::new(seq, point)),
            _ => merged,
        }
    })
}

/// The scopes a restore of `current` to `target` needs, as a merge does (design 7.7): `edit:character`, plus
/// `edit:scene` when anything outside `characters` changes. `merges` is bookkeeping and needs nothing of its own.
fn restore_scopes(current: &Value, target: &Value) -> Vec<Scope> {
    let outside = |tree: &Value| match tree {
        Value::Object(map) => map
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "characters" | "merges"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
        other => BTreeMap::from([(String::new(), other.clone())]),
    };
    let mut scopes = vec![Scope::EditCharacter];
    if outside(current) != outside(target) {
        scopes.push(Scope::EditScene);
    }
    scopes
}

/// Whether `by` is a person (or Omuzaki itself), who may do everything in the table of design 8.2. Agents and plugins
/// may do only what the table gives them.
fn is_person(by: &Actor) -> bool {
    matches!(by, Actor::Person { .. } | Actor::System)
}

#[derive(Default)]
struct StoreState {
    branches: Vec<BranchInfo>,
    points: BTreeMap<PointId, (Point, Arc<Value>)>,
    this: BranchId,
}

/// A [`BranchStore`] over branches and points a test puts in. Cheap to share: [`FakeBranches`] keeps its state here,
/// so what the fake does is what the ops read.
#[derive(Default)]
pub struct MemoryBranchStore {
    state: Mutex<StoreState>,
}

impl MemoryBranchStore {
    /// An empty store whose own branch is `main`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `branch`, or replaces the one with its id.
    pub fn put_branch(&self, branch: BranchInfo) {
        let mut state = lock(&self.state);
        match state.branches.iter_mut().find(|b| b.id == branch.id) {
            Some(known) => *known = branch,
            None => state.branches.push(branch),
        }
    }

    /// Stores `point` with the document tree of its snapshot, and returns its id.
    ///
    /// # Errors
    /// [`BranchError::Io`] if the point cannot be encoded.
    pub fn put_point(&self, point: Point, tree: Value) -> Result<PointId, BranchError> {
        let id = point.id()?;
        lock(&self.state).points.insert(id, (point, Arc::new(tree)));
        Ok(id)
    }

    /// Sets the branch [`BranchStore::this`] answers with: the branch of the lane the op runs on.
    pub fn set_this(&self, branch: BranchId) {
        lock(&self.state).this = branch;
    }

    /// The branch with `id`.
    pub fn get(&self, id: &BranchId) -> Option<BranchInfo> {
        lock(&self.state).branches.iter().find(|b| b.id == *id).cloned()
    }

    /// The highest point number branch `id` has used, 0 when it has none.
    fn last_seq(&self, id: &BranchId) -> u64 {
        lock(&self.state).points.values().filter(|(p, _)| p.branch == *id).map(|(p, _)| p.seq).max().unwrap_or(0)
    }
}

impl BranchStore for MemoryBranchStore {
    fn this(&self) -> Result<BranchInfo, BranchError> {
        let this = lock(&self.state).this.clone();
        self.get(&this).ok_or_else(|| BranchError::NotFound(this.to_string()))
    }

    fn list(&self) -> Result<Vec<BranchInfo>, BranchError> {
        Ok(lock(&self.state).branches.clone())
    }

    fn branch(&self, name: &BranchName) -> Result<BranchInfo, BranchError> {
        let state = lock(&self.state);
        let found = state.branches.iter().find(|b| b.name == *name && b.deleted.is_none());
        found.cloned().ok_or_else(|| BranchError::NotFound(name.to_string()))
    }

    fn point(&self, id: &PointId) -> Result<Point, BranchError> {
        lock(&self.state).points.get(id).map(|(p, _)| p.clone()).ok_or(BranchError::PointNotFound(*id))
    }

    fn tree(&self, id: &PointId) -> Result<Arc<Value>, BranchError> {
        lock(&self.state).points.get(id).map(|(_, t)| Arc::clone(t)).ok_or(BranchError::PointNotFound(*id))
    }
}

#[derive(Default)]
struct ViewState {
    now: Arc<Value>,
    txs: BTreeMap<u64, Arc<Value>>,
    snapshots: BTreeMap<SnapshotId, Arc<Value>>,
}

/// A [`HistoryView`] over states a test records: `now`, transactions and snapshots; points and branches resolve
/// through a [`MemoryBranchStore`], and the store's own branch resolves to `now`.
pub struct MemoryHistoryView {
    store: Arc<MemoryBranchStore>,
    state: Mutex<ViewState>,
}

impl MemoryHistoryView {
    /// A view whose live document is `now`.
    pub fn new(store: Arc<MemoryBranchStore>, now: Value) -> Self {
        Self { store, state: Mutex::new(ViewState { now: Arc::new(now), ..ViewState::default() }) }
    }

    /// Replaces the live document.
    pub fn set_now(&self, now: Value) {
        lock(&self.state).now = Arc::new(now);
    }

    /// Records the document as it was just after transaction `tx`.
    pub fn record_tx(&self, tx: TxId, tree: Value) {
        lock(&self.state).txs.insert(tx.0, Arc::new(tree));
    }

    /// Records a snapshot of `tree` and returns its id.
    ///
    /// # Errors
    /// [`BranchError::Io`] if the tree cannot be encoded.
    pub fn record_snapshot(&self, tree: Value) -> Result<SnapshotId, BranchError> {
        let id = snapshot_of(&tree)?;
        lock(&self.state).snapshots.insert(id, Arc::new(tree));
        Ok(id)
    }
}

impl HistoryView for MemoryHistoryView {
    fn tree(&self, at: &StateRef) -> Result<Arc<Value>, HistoryError> {
        let state = lock(&self.state);
        match at {
            StateRef::Now => Ok(Arc::clone(&state.now)),
            StateRef::Tx { tx, .. } => state.txs.get(&tx.0).cloned().ok_or(HistoryError::UnknownTx(*tx)),
            StateRef::Snapshot { id, .. } => {
                state.snapshots.get(id).cloned().ok_or(HistoryError::Snapshot(SnapshotError::NotFound(*id)))
            }
            StateRef::Point { id, .. } => Ok(self.store.tree(id)?),
            StateRef::Branch { name, .. } => {
                let branch = self.store.branch(name)?;
                if self.store.this()?.id == branch.id {
                    Ok(Arc::clone(&state.now))
                } else {
                    Ok(self.store.tree(&branch.head)?)
                }
            }
            _ => Err(HistoryError::Io("the mock does not know this kind of state reference".to_owned())),
        }
    }
}

/// The runner and sessions a test scripted for a lane.
type LaneParts = (Arc<dyn Runner>, Arc<dyn Sessions>);

/// What a point is made as.
#[derive(Copy, Clone)]
struct Made<'a> {
    kind: PointKind,
    label: &'a str,
    by: &'a Actor,
    advance: bool,
}

struct FakeState {
    folder: BranchId,
    docs: BTreeMap<BranchId, Value>,
    open: BTreeSet<BranchId>,
    busy: BTreeSet<BranchId>,
    lanes: BTreeMap<BranchId, LaneParts>,
    next_id: u64,
    subscribers: Vec<Sender<BranchEvent>>,
}

impl FakeState {
    fn emit(&mut self, event: &BranchEvent) {
        self.subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    }
}

/// A [`Branches`] lane manager in memory. Each branch has a document value the test sets; a checkpoint, fork or
/// switch records a point of it in the [`MemoryBranchStore`]. Ids are `0000000000000001`, `…02`, and so on; times
/// come from the injected clock. Lanes are not real: [`lane`](Branches::lane) hands out the runner and sessions a
/// test scripted with [`script_lane`](Self::script_lane), and those sessions are what the fake reads for how many
/// sessions work on a lane and which agent session an actor is (so who may rename, delete or ask for review).
/// Create makes no check of edit scopes; restore checks the ones it is given.
pub struct FakeBranches {
    store: Arc<MemoryBranchStore>,
    clock: Arc<dyn WallClock>,
    state: Mutex<FakeState>,
}

impl FakeBranches {
    /// A project with only `main`, whose document is `tree`, with its first point made by the system.
    ///
    /// # Errors
    /// [`BranchError::Io`] if the first point cannot be encoded.
    pub fn new(tree: Value, clock: Arc<dyn WallClock>) -> Result<Self, BranchError> {
        let store = Arc::new(MemoryBranchStore::new());
        let main = BranchId::main();
        let at = clock.now();
        let first = Point::new(main.clone(), 1, snapshot_of(&tree)?, Lsn(0), PointKind::Fork, Actor::System, at)
            .with_label("Start");
        let head = store.put_point(first, tree.clone())?;
        let mut info = BranchInfo::new(main.clone(), BranchName::main(), head, 1, at, Actor::System);
        info.lane = LaneState::open(true, 0);
        store.put_branch(info);
        let state = FakeState {
            folder: main.clone(),
            docs: BTreeMap::from([(main.clone(), tree)]),
            open: BTreeSet::from([main]),
            busy: BTreeSet::new(),
            lanes: BTreeMap::new(),
            next_id: 1,
            subscribers: Vec::new(),
        };
        Ok(Self { store, clock, state: Mutex::new(state) })
    }

    /// The store the fake keeps its branches and points in, for ops under test.
    pub fn store(&self) -> Arc<MemoryBranchStore> {
        Arc::clone(&self.store)
    }

    /// Sets `branch`'s live document: what its next checkpoint records.
    pub fn set_document(&self, branch: &BranchId, tree: Value) {
        lock(&self.state).docs.insert(branch.clone(), tree);
    }

    /// Marks whether `branch` has an open transaction, which makes a checkpoint of it or a switch away from it
    /// [`Busy`](BranchError::Busy).
    pub fn script_open_transaction(&self, branch: &BranchId, open: bool) {
        let mut state = lock(&self.state);
        if open {
            state.busy.insert(branch.clone());
        } else {
            state.busy.remove(branch);
        }
    }

    /// Sets what [`lane`](Branches::lane) hands out for `branch`.
    pub fn script_lane(&self, branch: &BranchId, runner: Arc<dyn Runner>, sessions: Arc<dyn Sessions>) {
        lock(&self.state).lanes.insert(branch.clone(), (runner, sessions));
    }

    fn live(&self, id: &BranchId) -> Result<BranchInfo, BranchError> {
        match self.store.get(id) {
            Some(info) if info.deleted.is_none() => Ok(info),
            _ => Err(BranchError::NotFound(id.to_string())),
        }
    }

    /// The session `by` runs, as far as the lanes a test scripted know it.
    fn session_of(state: &FakeState, by: &Actor) -> Option<SessionId> {
        state.lanes.values().find_map(|(_, sessions)| sessions.session_of(by)).map(|s| s.id)
    }

    /// Whether `branch` was made for the session `by` runs.
    fn made_for(state: &FakeState, by: &Actor, branch: &BranchInfo) -> bool {
        branch.owner_session.is_some() && branch.owner_session == Self::session_of(state, by)
    }

    /// Whether `by` works on `branch`: it was made for its session, or its session runs on the branch's lane.
    fn works_on(state: &FakeState, by: &Actor, branch: &BranchInfo) -> bool {
        Self::made_for(state, by, branch)
            || state.lanes.get(&branch.id).is_some_and(|(_, sessions)| sessions.session_of(by).is_some())
    }

    /// Refuses unless `by` is a person (design 8.2: switching the folder, sending back and marking merged).
    fn person_only(by: &Actor, what: &str) -> Result<(), BranchError> {
        if is_person(by) {
            return Ok(());
        }
        Err(BranchError::Denied(format!("{what} is for a person: ask the person to do it")))
    }

    /// Refuses unless `by` is a person, or the agent `branch` was made for (design 8.2: rename and delete).
    fn may_manage(state: &FakeState, by: &Actor, branch: &BranchInfo, what: &str) -> Result<(), BranchError> {
        if is_person(by) || (matches!(by, Actor::Agent { .. }) && Self::made_for(state, by, branch)) {
            return Ok(());
        }
        Err(BranchError::Denied(format!("an agent may {what} only a branch made for its session, not {}", branch.name)))
    }

    /// The session a new branch is made for. An agent makes branches only for its own session, and one it makes
    /// without saying is made for it (design 8.2).
    fn new_branch_owner(
        state: &FakeState,
        by: &Actor,
        asked: Option<SessionId>,
    ) -> Result<Option<SessionId>, BranchError> {
        if is_person(by) {
            return Ok(asked);
        }
        match (asked, Self::session_of(state, by)) {
            (Some(asked), Some(own)) if asked != own => {
                Err(BranchError::Denied("an agent makes a branch only for its own session".to_owned()))
            }
            (Some(_), None) => {
                Err(BranchError::Denied("no session of yours runs here to make a branch for".to_owned()))
            }
            (asked, own) => Ok(asked.or(own)),
        }
    }

    /// Refuses with [`Busy`](BranchError::Busy) while `branch` has an open transaction: what a checkpoint refuses, and
    /// so what anything that records the branch's state now must refuse before it changes anything.
    fn guard(state: &FakeState, branch: &BranchInfo) -> Result<(), BranchError> {
        if state.busy.contains(&branch.id) {
            return Err(BranchError::Busy(format!("branch {} has an open transaction", branch.name)));
        }
        Ok(())
    }

    /// `at` as the branch `source` itself reads it: a reference to `source` by name is its live document, not its
    /// head (design 6.1).
    fn normalized(&self, source: &BranchInfo, at: &StateRef) -> StateRef {
        match at {
            StateRef::Branch { name, .. } if self.store.branch(name).is_ok_and(|b| b.id == source.id) => StateRef::Now,
            other => other.clone(),
        }
    }

    /// Records a point of `tree` on `branch`. With `advance` the branch's head moves there; without, the point is only
    /// stored (a fork from an older state must not move the source branch back). The point includes what `tree`
    /// records as merged in and what the branch started with (design 5.3), and lists as parents the branch's previous
    /// point and the points merged in since it.
    fn record(
        &self,
        state: &FakeState,
        branch: &BranchInfo,
        tree: &Value,
        made: Made<'_>,
    ) -> Result<Point, BranchError> {
        let seq = self.store.last_seq(&branch.id) + 1;
        let snapshot = snapshot_of(tree)?;
        let merged = merges_of(tree, &branch.id);
        let before = self.store.point(&branch.head).map(|p| p.contains).unwrap_or_default();
        let mut parents = vec![branch.head];
        parents.extend(merged.iter().filter(|(b, v)| !before.includes(b, v)).map(|(_, v)| v.point));
        let point =
            Point::new(branch.id.clone(), seq, snapshot, Lsn(seq), made.kind, made.by.clone(), self.clock.now())
                .with_parents(parents)
                .with_contains(merged.join(&branch.inherits))
                .with_label(made.label);
        let id = self.store.put_point(point.clone(), tree.clone())?;
        if !made.advance {
            return Ok(point);
        }
        let mut moved = branch.clone();
        moved.head = id;
        moved.seq = seq;
        moved.lane = Self::lane_state(state, &moved.id);
        self.store.put_branch(moved);
        Ok(point)
    }

    /// How many sessions work on `id`'s lane: the scripted ones that have not ended.
    fn active_sessions(state: &FakeState, id: &BranchId) -> u32 {
        let count = state.lanes.get(id).map_or(0, |(_, sessions)| {
            sessions.sessions().iter().filter(|s| !matches!(s.state, SessionState::Ended { .. })).count()
        });
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn lane_state(state: &FakeState, id: &BranchId) -> LaneState {
        if state.open.contains(id) {
            LaneState::open(state.folder == *id, Self::active_sessions(state, id))
        } else {
            LaneState::Closed
        }
    }

    /// Brings every branch's recorded lane up to date: which lanes are open, and how many sessions work on each.
    fn sync_lanes(&self, state: &FakeState) {
        for mut info in self.store.list().unwrap_or_default() {
            let lane = Self::lane_state(state, &info.id);
            if info.lane != lane {
                info.lane = lane;
                self.store.put_branch(info);
            }
        }
    }

    fn resolve(&self, state: &FakeState, source: &BranchInfo, at: &StateRef) -> Result<Value, BranchError> {
        match at {
            StateRef::Now => Ok(state.docs.get(&source.id).cloned().unwrap_or_default()),
            StateRef::Point { id, .. } => Ok(self.store.tree(id)?.as_ref().clone()),
            StateRef::Branch { name, .. } => Ok(self.store.tree(&self.store.branch(name)?.head)?.as_ref().clone()),
            StateRef::Tx { .. } | StateRef::Snapshot { .. } => Err(BranchError::Invalid(
                "the fake resolves `now`, points and branches; record transactions and snapshots in a MemoryHistoryView"
                    .to_owned(),
            )),
            _ => Err(BranchError::Invalid("the fake does not know this kind of state reference".to_owned())),
        }
    }

    fn open_lane(&self, state: &mut FakeState, id: &BranchId) -> Result<(), BranchError> {
        if state.open.contains(id) {
            return Ok(());
        }
        if state.open.len() >= MAX_OPEN_LANES {
            return Err(BranchError::LaneLimit);
        }
        state.open.insert(id.clone());
        if let Some(mut info) = self.store.get(id) {
            info.lane = Self::lane_state(state, id);
            self.store.put_branch(info);
        }
        state.emit(&BranchEvent::lane_opened(id.clone()));
        Ok(())
    }

    fn close_lane(&self, state: &mut FakeState, id: &BranchId) {
        if state.open.remove(id) {
            if let Some(mut info) = self.store.get(id) {
                info.lane = LaneState::Closed;
                self.store.put_branch(info);
            }
            state.emit(&BranchEvent::lane_closed(id.clone()));
        }
    }

    fn name_taken(&self, name: &BranchName, except: Option<&BranchId>) -> bool {
        let branches = self.store.list().unwrap_or_default();
        branches.iter().any(|b| b.name == *name && b.deleted.is_none() && Some(&b.id) != except)
    }
}

/// `yyyy-mm-dd-hhmm` and `hh:mm` of a UTC time in milliseconds, for the names of automatic branches.
fn stamp(time: WallTime) -> (String, String) {
    let minutes = time.0 / 60_000;
    let (days, minute_of_day) = (minutes / 1440, minutes % 1440);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    let (hour, minute) = (minute_of_day / 60, minute_of_day % 60);
    (format!("{year:04}-{month:02}-{day:02}-{hour:02}{minute:02}"), format!("{hour:02}:{minute:02}"))
}

impl Branches for FakeBranches {
    fn list(&self) -> Vec<BranchInfo> {
        let state = lock(&self.state);
        self.sync_lanes(&state);
        self.store.list().unwrap_or_default()
    }

    fn folder(&self) -> BranchId {
        lock(&self.state).folder.clone()
    }

    fn create(&self, new: NewBranch, by: &Actor) -> Result<BranchInfo, BranchError> {
        let mut state = lock(&self.state);
        let source = self.live(&new.from_branch)?;
        let owner = Self::new_branch_owner(&state, by, new.for_session)?;
        if self.name_taken(&new.name, None) {
            return Err(BranchError::NameTaken(new.name));
        }
        let at = self.normalized(&source, &new.at);
        if at == StateRef::Now {
            // Forking from now records the source's state now, which a checkpoint would refuse to do.
            Self::guard(&state, &source)?;
        }
        let tree = self.resolve(&state, &source, &at)?;
        let id = BranchId::new(format!("{:016x}", state.next_id + 1)).map_err(|e| BranchError::Io(e.to_string()))?;
        // The fork point is a point of the source: a head that another branch made (a branch forked without edits has
        // the source's point as its head) cannot stand in for it, or the new branch would name a version of the source
        // that carries another branch's number.
        let head = self.store.point(&source.head)?;
        let fork = if head.branch == source.id && *self.store.tree(&source.head)? == tree {
            head
        } else {
            let label = format!("Fork {}", new.name);
            let made = Made { kind: PointKind::Fork, label: &label, by, advance: at == StateRef::Now };
            self.record(&state, &source, &tree, made)?
        };
        let fork_id = fork.id()?;
        state.next_id += 1;
        let mut info = BranchInfo::new(id.clone(), new.name, fork_id, fork.seq, self.clock.now(), by.clone());
        info.title = new.title;
        info.from = Some(fork_id);
        info.inherits = fork.contains.join(&Contains::new().with(source.id.clone(), Version::new(fork.seq, fork_id)));
        info.focus = new.focus;
        info.owner_session = owner;
        state.docs.insert(id, tree);
        self.store.put_branch(info.clone());
        state.emit(&BranchEvent::created(info.clone()));
        Ok(info)
    }

    fn checkpoint(
        &self,
        branch: &BranchId,
        label: &str,
        ready: Option<String>,
        by: &Actor,
    ) -> Result<PointId, BranchError> {
        let mut state = lock(&self.state);
        let info = self.live(branch)?;
        if ready.is_some() && !is_person(by) && !Self::works_on(&state, by, &info) {
            return Err(BranchError::Denied(format!(
                "an agent asks for review only on a branch it works on, not {}",
                info.name
            )));
        }
        Self::guard(&state, &info)?;
        let tree = state.docs.get(branch).cloned().unwrap_or_default();
        if ready.is_none() && *self.store.tree(&info.head)? == tree {
            return Ok(info.head);
        }
        let kind = if ready.is_some() { PointKind::Ready } else { PointKind::Checkpoint };
        let point = self.record(&state, &info, &tree, Made { kind, label, by, advance: true })?;
        let id = point.id()?;
        let is_ready = ready.is_some();
        if is_ready && let Some(mut updated) = self.store.get(branch) {
            updated.review = Some(Review::new(id, by.clone(), self.clock.now(), ready));
            self.store.put_branch(updated);
        }
        state.emit(&BranchEvent::checkpointed(branch.clone(), id, is_ready));
        if is_ready {
            state.emit(&BranchEvent::review_requested(branch.clone(), id));
        }
        Ok(id)
    }

    fn switch(&self, to: &BranchId, by: &Actor) -> Result<Switched, BranchError> {
        Self::person_only(by, "switching the folder")?;
        let mut state = lock(&self.state);
        self.live(to)?;
        let from = state.folder.clone();
        if from == *to {
            return Ok(Switched::new(from, to.clone()));
        }
        if state.busy.contains(&from) {
            return Err(BranchError::Busy("finish or cancel the open transaction first".to_owned()));
        }
        self.open_lane(&mut state, to)?;
        let old = self.live(&from)?;
        let tree = state.docs.get(&from).cloned().unwrap_or_default();
        let made = Made { kind: PointKind::Switch, label: "Switched away", by, advance: true };
        self.record(&state, &old, &tree, made)?;
        state.folder = to.clone();
        self.store.set_this(to.clone());
        // The old folder lane stays open as a side lane while sessions work on it (design 5.5).
        if Self::active_sessions(&state, &from) == 0 {
            self.close_lane(&mut state, &from);
        }
        self.sync_lanes(&state);
        state.emit(&BranchEvent::switched(from.clone(), to.clone()));
        Ok(Switched::new(from, to.clone()))
    }

    fn rename(
        &self,
        branch: &BranchId,
        name: BranchName,
        title: Option<String>,
        by: &Actor,
    ) -> Result<BranchInfo, BranchError> {
        let mut state = lock(&self.state);
        let mut info = self.live(branch)?;
        Self::may_manage(&state, by, &info, "rename")?;
        if branch.is_main() {
            return Err(BranchError::Refused("`main` keeps its name".to_owned()));
        }
        if self.name_taken(&name, Some(branch)) {
            return Err(BranchError::NameTaken(name));
        }
        info.name = name.clone();
        info.title = title;
        self.store.put_branch(info.clone());
        state.emit(&BranchEvent::renamed(branch.clone(), name));
        Ok(info)
    }

    fn delete(&self, branch: &BranchId, by: &Actor) -> Result<(), BranchError> {
        let mut state = lock(&self.state);
        let mut info = self.live(branch)?;
        Self::may_manage(&state, by, &info, "delete")?;
        if branch.is_main() || state.folder == *branch || state.open.contains(branch) {
            return Err(BranchError::Refused(
                "`main`, the folder branch and a branch with an open lane cannot be deleted".to_owned(),
            ));
        }
        info.deleted = Some(self.clock.now());
        self.store.put_branch(info);
        state.emit(&BranchEvent::deleted(branch.clone()));
        Ok(())
    }

    fn undelete(&self, branch: &BranchId, by: &Actor) -> Result<BranchInfo, BranchError> {
        let mut state = lock(&self.state);
        let mut info = match self.store.get(branch) {
            Some(info) if info.deleted.is_some() => info,
            _ => return Err(BranchError::NotFound(branch.to_string())),
        };
        // The table of design 8.2 is silent on the bin; bringing a branch back undoes a delete, so it follows delete.
        Self::may_manage(&state, by, &info, "bring back")?;
        if self.name_taken(&info.name, Some(branch)) {
            return Err(BranchError::NameTaken(info.name));
        }
        info.deleted = None;
        self.store.put_branch(info.clone());
        state.emit(&BranchEvent::undeleted(branch.clone()));
        Ok(info)
    }

    fn review(&self, branch: &BranchId, decision: ReviewDecision, by: &Actor) -> Result<BranchInfo, BranchError> {
        Self::person_only(by, "deciding a review")?;
        let mut state = lock(&self.state);
        let mut info = self.live(branch)?;
        let waiting = info.review.as_mut().filter(|r| r.state == ReviewState::Ready);
        let Some(review) = waiting else {
            return Err(BranchError::Refused(format!("branch {} has no review waiting", info.name)));
        };
        review.state = match decision {
            ReviewDecision::MarkMerged => ReviewState::Merged,
            ReviewDecision::SendBack { .. } => ReviewState::SentBack,
            _ => return Err(BranchError::Invalid("the fake does not know this kind of review decision".to_owned())),
        };
        let decided = review.state;
        self.store.put_branch(info.clone());
        state.emit(&BranchEvent::reviewed(branch.clone(), decided));
        Ok(info)
    }

    fn restore(&self, branch: &BranchId, to: StateRef, by: &Actor, grants: &Grants) -> Result<Restored, BranchError> {
        let mut state = lock(&self.state);
        let info = self.live(branch)?;
        let to = self.normalized(&info, &to);
        if to == StateRef::Now {
            return Err(BranchError::Invalid("restoring to `now` changes nothing".to_owned()));
        }
        // Everything that can refuse comes before the first change, so a refused restore leaves the lane as it was.
        Self::guard(&state, &info)?;
        let target = self.resolve(&state, &info, &to)?;
        let current = state.docs.get(branch).cloned().unwrap_or_default();
        if let Some(missing) = restore_scopes(&current, &target).into_iter().find(|scope| !grants.allows(*scope)) {
            return Err(BranchError::Denied(format!("this restore changes what the `{missing}` scope covers")));
        }
        let (slug, clock_time) = stamp(self.clock.now());
        let mut name = format!("restore/{slug}");
        let mut suffix = 1;
        while self.name_taken(&BranchName::new(name.clone())?, None) {
            suffix += 1;
            name = format!("restore/{slug}-{suffix}");
        }
        let backup_name = BranchName::new(name)?;
        let id = BranchId::new(format!("{:016x}", state.next_id + 1)).map_err(|e| BranchError::Io(e.to_string()))?;
        let made = Made { kind: PointKind::Restore, label: "Before restore", by, advance: true };
        let point = self.record(&state, &info, &current, made)?;
        let point_id = point.id()?;
        state.next_id += 1;
        let mut backup = BranchInfo::new(id.clone(), backup_name, point_id, point.seq, self.clock.now(), by.clone());
        backup.title = Some(format!("Before restore {clock_time}"));
        backup.from = Some(point_id);
        backup.inherits = point.contains.join(&Contains::new().with(branch.clone(), Version::new(point.seq, point_id)));
        backup.auto = true;
        self.store.put_branch(backup.clone());
        state.docs.insert(id, current);
        state.docs.insert(branch.clone(), target);
        state.emit(&BranchEvent::created(backup.clone()));
        Ok(Restored::new(backup, to))
    }

    fn lane(&self, branch: &BranchId) -> Result<Lane, BranchError> {
        let mut state = lock(&self.state);
        self.live(branch)?;
        let scripted = state.lanes.get(branch).map(|(r, s)| (Arc::clone(r), Arc::clone(s)));
        let Some((runner, sessions)) = scripted else {
            return Err(BranchError::Invalid(
                "no runner scripted for this lane: use FakeBranches::script_lane".to_owned(),
            ));
        };
        self.open_lane(&mut state, branch)?;
        self.sync_lanes(&state);
        Ok(Lane::new(runner, sessions))
    }

    fn subscribe(&self) -> Receiver<BranchEvent> {
        let (tx, rx) = channel();
        lock(&self.state).subscribers.push(tx);
        rx
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use omuzaki_core::time::Seconds;
    use omuzaki_doc_api::DocPath;
    use omuzaki_history_api::branch::MAIN_BRANCH;
    use omuzaki_ops_api::{
        ApprovalId, Budget, Decision, EndReason, Ended, Event, Limits, OpError, OpInfo, OpRecord, Outcome,
        PendingApproval, Request, Resolution, SessionInfo, SessionSpec, SessionUndo, Spend, Transport,
    };
    use serde_json::json;

    use super::*;
    use crate::FakeWallClock;

    type Test = Result<(), Box<dyn Error>>;

    fn person() -> Actor {
        Actor::Person { name: "Sam".to_owned() }
    }

    fn agent() -> Actor {
        Actor::Agent { id: "claude-code#12".to_owned(), model: None }
    }

    fn fake() -> Result<(FakeBranches, Arc<FakeWallClock>), BranchError> {
        // 2025-10-12 14:32 UTC.
        let clock = Arc::new(FakeWallClock::new(WallTime(1_760_279_520_000)));
        Ok((FakeBranches::new(json!({"face": {"jaw": 0.5}}), clock.clone())?, clock))
    }

    fn name(text: &str) -> Result<BranchName, Box<dyn Error>> {
        Ok(BranchName::new(text)?)
    }

    fn fork(fake: &FakeBranches, text: &str, by: &Actor) -> Result<BranchInfo, Box<dyn Error>> {
        Ok(fake.create(NewBranch::new(name(text)?, BranchId::main()), by)?)
    }

    #[test]
    fn a_new_project_has_main_open_as_the_folder() -> Test {
        let (fake, _) = fake()?;
        let all = fake.list();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id.as_str(), MAIN_BRANCH);
        assert_eq!(all[0].lane, LaneState::open(true, 0));
        assert_eq!(fake.folder(), BranchId::main());
        assert_eq!(fake.store().this()?.id, BranchId::main());
        Ok(())
    }

    #[test]
    fn create_forks_from_a_checkpoint_of_the_source_and_inherits_what_it_contained() -> Test {
        let (fake, _) = fake()?;
        let events = fake.subscribe();
        fake.set_document(&BranchId::main(), json!({"face": {"jaw": 0.7}}));
        let new = NewBranch::new(name("aria/oval-face")?, BranchId::main())
            .with_title("Oval face")
            .with_focus(vec!["characters/aria".to_owned()])
            .for_session(SessionId(12));
        let info = fake.create(new, &person())?;
        assert_eq!(info.id.as_str(), "0000000000000002");
        assert_eq!(info.from, Some(info.head));
        assert_eq!(info.title.as_deref(), Some("Oval face"));
        assert_eq!(info.focus, vec!["characters/aria".to_owned()]);
        assert_eq!(info.owner_session, Some(SessionId(12)));
        assert!(!info.auto);
        let store = fake.store();
        let fork_point = store.point(&info.head)?;
        assert_eq!(
            (fork_point.kind, fork_point.branch.clone(), fork_point.seq),
            (PointKind::Fork, BranchId::main(), 2)
        );
        assert_eq!(
            *store.tree(&info.head)?,
            json!({"face": {"jaw": 0.7}}),
            "the source's live document was checkpointed"
        );
        assert_eq!(info.inherits.get(&BranchId::main()), Some(&Version::new(2, info.head)));
        assert_eq!(events.try_recv()?, BranchEvent::created(info));
        Ok(())
    }

    #[test]
    fn forking_an_unchanged_source_reuses_its_head() -> Test {
        let (fake, _) = fake()?;
        let head = fake.store().this()?.head;
        let info = fork(&fake, "same", &person())?;
        assert_eq!(info.head, head);
        assert_eq!(fake.store().last_seq(&BranchId::main()), 1, "nothing new was written");
        Ok(())
    }

    #[test]
    fn create_from_a_point_and_from_a_branch() -> Test {
        let (fake, _) = fake()?;
        let first = fake.store().this()?.head;
        fake.set_document(&BranchId::main(), json!({"v": 2}));
        let second = fake.checkpoint(&BranchId::main(), "v2", None, &person())?;
        let from_point =
            fake.create(NewBranch::new(name("old")?, BranchId::main()).at(StateRef::point(first)), &person())?;
        assert_eq!(*fake.store().tree(&from_point.head)?, json!({"face": {"jaw": 0.5}}));
        let main_head = fake.store().this()?.head;
        assert_eq!(main_head, second, "forking from an older state did not move main's head");
        let from_branch = fake.create(
            NewBranch::new(name("copy")?, BranchId::main()).at(StateRef::branch(BranchName::main())),
            &person(),
        )?;
        assert_eq!(from_branch.head, main_head, "the head of main, which already holds that state");
        let by_tx = NewBranch::new(name("rewind")?, BranchId::main()).at(StateRef::tx(TxId(3)));
        assert!(matches!(fake.create(by_tx, &person()), Err(BranchError::Invalid(_))));
        Ok(())
    }

    #[test]
    fn create_refuses_a_taken_name_and_an_unknown_source() -> Test {
        let (fake, _) = fake()?;
        fork(&fake, "aria/oval-face", &person())?;
        assert_eq!(
            fork(&fake, "aria/oval-face", &person()).err().map(|e| e.to_string()),
            Some("the name aria/oval-face is taken".to_owned())
        );
        let main_again = fork(&fake, "main", &person());
        assert!(main_again.is_err(), "`main` names only the first branch");
        let unknown = BranchId::new("00000000000000ff")?;
        assert!(matches!(fake.create(NewBranch::new(name("x")?, unknown), &person()), Err(BranchError::NotFound(_))));
        Ok(())
    }

    #[test]
    fn checkpoint_makes_a_point_that_chains_to_the_head() -> Test {
        let (fake, _) = fake()?;
        let events = fake.subscribe();
        let before = fake.store().this()?.head;
        fake.set_document(&BranchId::main(), json!({"v": 2}));
        let id = fake.checkpoint(&BranchId::main(), "Jaw rounder", None, &agent())?;
        let point = fake.store().point(&id)?;
        assert_eq!(
            (point.kind, point.seq, point.parents.clone(), point.label.clone()),
            (PointKind::Checkpoint, 2, vec![before], "Jaw rounder".to_owned())
        );
        assert_eq!(point.by, agent());
        assert_eq!(fake.store().this()?.head, id);
        assert_eq!(events.try_recv()?, BranchEvent::checkpointed(BranchId::main(), id, false));
        assert_eq!(
            fake.checkpoint(&BranchId::main(), "again", None, &agent())?,
            id,
            "unchanged: the head, nothing written"
        );
        assert!(events.try_recv().is_err(), "and no event");
        Ok(())
    }

    #[test]
    fn a_checkpoint_waits_for_an_open_transaction() -> Test {
        let (fake, _) = fake()?;
        fake.set_document(&BranchId::main(), json!({"v": 2}));
        fake.script_open_transaction(&BranchId::main(), true);
        assert!(matches!(fake.checkpoint(&BranchId::main(), "x", None, &person()), Err(BranchError::Busy(_))));
        fake.script_open_transaction(&BranchId::main(), false);
        assert!(fake.checkpoint(&BranchId::main(), "x", None, &person()).is_ok());
        Ok(())
    }

    #[test]
    fn a_ready_checkpoint_asks_for_review_and_the_person_decides() -> Test {
        let (fake, _) = fake()?;
        agent_on_main(&fake);
        let aria = fork(&fake, "aria/oval-face", &agent())?;
        fake.set_document(&aria.id, json!({"v": "oval"}));
        let events = fake.subscribe();
        let point = fake.checkpoint(&aria.id, "Oval face, version A", Some("rounder chin".to_owned()), &agent())?;
        assert_eq!(events.try_recv()?, BranchEvent::checkpointed(aria.id.clone(), point, true));
        assert_eq!(events.try_recv()?, BranchEvent::review_requested(aria.id.clone(), point));
        let waiting = fake.store().get(&aria.id).and_then(|b| b.review).ok_or("no review")?;
        assert_eq!(
            (waiting.point, waiting.state, waiting.note.as_deref()),
            (point, ReviewState::Ready, Some("rounder chin"))
        );
        assert_eq!(fake.store().point(&point)?.kind, PointKind::Ready);

        let sent = fake.review(&aria.id, ReviewDecision::send_back("narrower jaw"), &person())?;
        assert_eq!(sent.review.map(|r| r.state), Some(ReviewState::SentBack));
        assert_eq!(events.try_recv()?, BranchEvent::reviewed(aria.id.clone(), ReviewState::SentBack));
        assert!(
            matches!(fake.review(&aria.id, ReviewDecision::MarkMerged, &person()), Err(BranchError::Refused(_))),
            "nothing waits now"
        );

        fake.set_document(&aria.id, json!({"v": "oval 2"}));
        fake.checkpoint(&aria.id, "Version B", Some(String::new()), &agent())?;
        let merged = fake.review(&aria.id, ReviewDecision::MarkMerged, &person())?;
        assert_eq!(merged.review.map(|r| r.state), Some(ReviewState::Merged));
        Ok(())
    }

    #[test]
    fn a_ready_checkpoint_of_an_unchanged_branch_still_makes_the_review_point() -> Test {
        let (fake, _) = fake()?;
        agent_on_main(&fake);
        let head = fake.store().this()?.head;
        let point = fake.checkpoint(&BranchId::main(), "ready", Some("note".to_owned()), &agent())?;
        assert_ne!(point, head);
        Ok(())
    }

    #[test]
    fn switch_moves_the_folder_and_the_lanes() -> Test {
        let (fake, _) = fake()?;
        let aria = fork(&fake, "aria/oval-face", &person())?;
        let events = fake.subscribe();
        let switched = fake.switch(&aria.id, &person())?;
        assert_eq!(switched, Switched::new(BranchId::main(), aria.id.clone()));
        assert_eq!(fake.folder(), aria.id);
        assert_eq!(fake.store().this()?.id, aria.id);
        let lanes: Vec<(String, LaneState)> = fake.list().into_iter().map(|b| (b.id.to_string(), b.lane)).collect();
        assert_eq!(
            lanes,
            vec![("main".to_owned(), LaneState::Closed), (aria.id.to_string(), LaneState::open(true, 0))]
        );
        let seen: Vec<BranchEvent> = events.try_iter().collect();
        assert_eq!(
            seen,
            vec![
                BranchEvent::lane_opened(aria.id.clone()),
                BranchEvent::lane_closed(BranchId::main()),
                BranchEvent::switched(BranchId::main(), aria.id.clone()),
            ]
        );
        let switch_point = fake.store().get(&BranchId::main()).ok_or("main")?.head;
        assert_eq!(fake.store().point(&switch_point)?.kind, PointKind::Switch);
        assert_eq!(fake.switch(&aria.id, &person())?, Switched::new(aria.id.clone(), aria.id.clone()), "already there");
        assert!(events.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn switch_is_refused_for_an_open_transaction_an_unknown_branch_and_too_many_lanes() -> Test {
        let (fake, _) = fake()?;
        let aria = fork(&fake, "aria", &person())?;
        fake.script_open_transaction(&BranchId::main(), true);
        assert!(matches!(fake.switch(&aria.id, &person()), Err(BranchError::Busy(_))));
        assert_eq!(fake.folder(), BranchId::main(), "nothing moved");
        fake.script_open_transaction(&BranchId::main(), false);
        assert!(matches!(fake.switch(&BranchId::new("00000000000000ff")?, &person()), Err(BranchError::NotFound(_))));

        // Open every lane there is room for, then try one more.
        let mut others = Vec::new();
        for n in 0..MAX_OPEN_LANES {
            others.push(fork(&fake, &format!("b{n}"), &person())?);
        }
        let runner: Arc<dyn Runner> = Arc::new(NoRunner);
        let sessions: Arc<dyn Sessions> = Arc::new(NoSessions);
        let mut result = Ok(());
        for branch in &others {
            fake.script_lane(&branch.id, runner.clone(), sessions.clone());
            result = fake.lane(&branch.id).map(|_| ());
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(BranchError::LaneLimit));
        assert_eq!(fake.list().iter().filter(|b| b.lane != LaneState::Closed).count(), MAX_OPEN_LANES);
        Ok(())
    }

    struct NoRunner;
    struct NoSessions;

    fn unused() -> OpError {
        OpError::unavailable("test lane")
    }

    impl Runner for NoRunner {
        fn ops(&self) -> Vec<OpInfo> {
            Vec::new()
        }
        fn run(&self, _: Request) -> Result<Outcome, OpError> {
            Err(unused())
        }
        fn begin(&self, _: &str, _: &Actor) -> TxId {
            TxId(0)
        }
        fn commit(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
            Err(unused())
        }
        fn rollback(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
            Err(unused())
        }
        fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
            Err(unused())
        }
        fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
            Err(unused())
        }
        fn subscribe(&self, _: &Grants) -> Result<Receiver<Event>, OpError> {
            Err(unused())
        }
        fn get(&self, _: &DocPath, _: &Grants) -> Result<Option<Value>, OpError> {
            Err(unused())
        }
        fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<OpRecord>, OpError> {
            Err(unused())
        }
    }

    impl Sessions for NoSessions {
        fn open(&self, _: SessionSpec) -> Result<SessionInfo, OpError> {
            Err(unused())
        }
        fn sessions(&self) -> Vec<SessionInfo> {
            Vec::new()
        }
        fn session(&self, _: SessionId) -> Option<SessionInfo> {
            None
        }
        fn session_of(&self, _: &Actor) -> Option<SessionInfo> {
            None
        }
        fn pause(&self, _: SessionId, _: &Actor) -> Result<SessionInfo, OpError> {
            Err(unused())
        }
        fn resume(&self, _: SessionId, _: &Actor) -> Result<SessionInfo, OpError> {
            Err(unused())
        }
        fn set_grants(&self, _: SessionId, _: Grants, _: &Actor) -> Result<SessionInfo, OpError> {
            Err(unused())
        }
        fn set_budget(&self, _: SessionId, _: Budget, _: &Actor) -> Result<SessionInfo, OpError> {
            Err(unused())
        }
        fn charge(&self, _: SessionId, _: Spend) -> Result<SessionInfo, OpError> {
            Err(unused())
        }
        fn end(&self, _: SessionId, _: EndReason) -> Result<Ended, OpError> {
            Err(unused())
        }
        fn undo_session(&self, _: SessionId, _: &Actor) -> Result<SessionUndo, OpError> {
            Err(unused())
        }
        fn approvals(&self) -> Vec<PendingApproval> {
            Vec::new()
        }
        fn resolve(&self, _: ApprovalId, _: Decision, _: &Actor) -> Result<Resolution, OpError> {
            Err(unused())
        }
    }

    #[test]
    fn rename_delete_and_undelete_keep_names_unique() -> Test {
        let (fake, _) = fake()?;
        let a = fork(&fake, "a", &person())?;
        let b = fork(&fake, "b", &person())?;
        assert!(matches!(fake.rename(&b.id, name("a")?, None, &person()), Err(BranchError::NameTaken(_))));
        let renamed = fake.rename(&b.id, name("b2")?, Some("Bee".to_owned()), &person())?;
        assert_eq!((renamed.name.as_str(), renamed.title.as_deref()), ("b2", Some("Bee")));
        assert!(matches!(
            fake.rename(&BranchId::main(), name("trunk")?, None, &person()),
            Err(BranchError::Refused(_))
        ));

        fake.delete(&a.id, &person())?;
        assert!(fake.list().iter().any(|x| x.id == a.id && x.deleted.is_some()), "in the bin, not gone");
        assert!(fake.store().branch(&name("a")?).is_err(), "a deleted branch's name is free");
        fork(&fake, "a", &person())?;
        assert!(matches!(fake.undelete(&a.id, &person()), Err(BranchError::NameTaken(_))));
        fake.rename(&fake.list().last().ok_or("branch")?.id, name("a-new")?, None, &person())?;
        assert_eq!(fake.undelete(&a.id, &person())?.deleted, None);
        assert!(matches!(fake.undelete(&a.id, &person()), Err(BranchError::NotFound(_))), "not in the bin");
        Ok(())
    }

    #[test]
    fn delete_refuses_main_the_folder_and_an_open_lane() -> Test {
        let (fake, _) = fake()?;
        let aria = fork(&fake, "aria", &person())?;
        assert!(matches!(fake.delete(&BranchId::main(), &person()), Err(BranchError::Refused(_))));
        fake.switch(&aria.id, &person())?;
        assert!(matches!(fake.delete(&aria.id, &person()), Err(BranchError::Refused(_))), "the folder branch");
        Ok(())
    }

    #[test]
    fn restore_keeps_the_abandoned_future_on_an_automatic_branch() -> Test {
        let (fake, _) = fake()?;
        let start = fake.store().this()?.head;
        fake.set_document(&BranchId::main(), json!({"v": "later"}));
        let events = fake.subscribe();
        let restored = fake.restore(&BranchId::main(), StateRef::point(start), &person(), &Grants::person())?;
        let backup = &restored.backup;
        assert!(backup.auto);
        assert_eq!(backup.name.as_str(), "restore/2025-10-12-1432");
        assert_eq!(backup.title.as_deref(), Some("Before restore 14:32"));
        assert_eq!(*fake.store().tree(&backup.head)?, json!({"v": "later"}), "the abandoned state is kept");
        assert_eq!(fake.store().point(&backup.head)?.kind, PointKind::Restore);
        assert_eq!(events.try_recv()?, BranchEvent::created(backup.clone()));
        // The restored document is what the next checkpoint records.
        let next = fake.checkpoint(&BranchId::main(), "after", None, &person())?;
        assert_eq!(*fake.store().tree(&next)?, json!({"face": {"jaw": 0.5}}));
        // A second restore in the same minute gets a suffix.
        fake.set_document(&BranchId::main(), json!({"v": "again"}));
        let second = fake.restore(&BranchId::main(), StateRef::point(start), &person(), &Grants::person())?;
        assert_eq!(second.backup.name.as_str(), "restore/2025-10-12-1432-2");
        Ok(())
    }

    #[test]
    fn restore_refuses_now_and_a_caller_without_the_edit_scope() -> Test {
        let (fake, _) = fake()?;
        let start = fake.store().this()?.head;
        assert!(matches!(
            fake.restore(&BranchId::main(), StateRef::Now, &person(), &Grants::person()),
            Err(BranchError::Invalid(_))
        ));
        let none = Grants::of([]);
        assert!(matches!(
            fake.restore(&BranchId::main(), StateRef::point(start), &person(), &none),
            Err(BranchError::Denied(_))
        ));
        Ok(())
    }

    #[test]
    fn lane_needs_a_scripted_runner_and_opens_the_branch() -> Test {
        let (fake, _) = fake()?;
        let aria = fork(&fake, "aria", &person())?;
        assert!(matches!(fake.lane(&aria.id), Err(BranchError::Invalid(_))));
        fake.script_lane(&aria.id, Arc::new(NoRunner), Arc::new(NoSessions));
        let events = fake.subscribe();
        assert!(fake.lane(&aria.id).is_ok());
        assert_eq!(events.try_recv()?, BranchEvent::lane_opened(aria.id.clone()));
        assert_eq!(fake.store().get(&aria.id).map(|b| b.lane), Some(LaneState::open(false, 0)));
        assert!(fake.lane(&aria.id).is_ok());
        assert!(events.try_recv().is_err(), "already open");
        Ok(())
    }

    #[test]
    fn a_dropped_subscriber_does_not_stop_the_others() -> Test {
        let (fake, _) = fake()?;
        drop(fake.subscribe());
        let kept = fake.subscribe();
        fork(&fake, "aria", &person())?;
        assert!(matches!(kept.try_recv(), Ok(BranchEvent::Created { .. })));
        Ok(())
    }

    #[test]
    fn the_store_answers_by_name_point_and_tree() -> Test {
        let (fake, _) = fake()?;
        let store = fake.store();
        let main = store.branch(&BranchName::main())?;
        assert_eq!(store.list()?.len(), 1);
        assert_eq!(store.point(&main.head)?.seq, 1);
        assert_eq!(*store.tree(&main.head)?, json!({"face": {"jaw": 0.5}}));
        let missing = PointId(ContentHash::of(b"missing"));
        assert_eq!(store.point(&missing), Err(BranchError::PointNotFound(missing)));
        assert_eq!(store.tree(&missing).err(), Some(BranchError::PointNotFound(missing)));
        assert!(matches!(store.branch(&name("nope")?), Err(BranchError::NotFound(_))));
        Ok(())
    }

    #[test]
    fn the_history_view_resolves_every_state_reference() -> Test {
        let (fake, _) = fake()?;
        let aria = fork(&fake, "aria", &person())?;
        let store = fake.store();
        let view = MemoryHistoryView::new(store.clone(), json!({"v": "now"}));
        view.record_tx(TxId(7), json!({"v": "tx7"}));
        let snapshot = view.record_snapshot(json!({"v": "snap"}))?;
        assert_eq!(*view.tree(&StateRef::Now)?, json!({"v": "now"}));
        assert_eq!(*view.tree(&StateRef::tx(TxId(7)))?, json!({"v": "tx7"}));
        assert_eq!(*view.tree(&StateRef::snapshot(snapshot))?, json!({"v": "snap"}));
        assert_eq!(*view.tree(&StateRef::point(aria.head))?, json!({"face": {"jaw": 0.5}}));
        assert_eq!(
            *view.tree(&StateRef::branch(BranchName::main()))?,
            json!({"v": "now"}),
            "the op's own branch is `now`"
        );
        assert_eq!(
            *view.tree(&StateRef::branch(aria.name.clone()))?,
            json!({"face": {"jaw": 0.5}}),
            "another branch is its head"
        );
        view.set_now(json!({"v": "newer"}));
        assert_eq!(*view.tree(&StateRef::Now)?, json!({"v": "newer"}));
        assert_eq!(view.tree(&StateRef::tx(TxId(8))).err(), Some(HistoryError::UnknownTx(TxId(8))));
        let missing = SnapshotId(ContentHash::of(b"missing"));
        assert_eq!(
            view.tree(&StateRef::snapshot(missing)).err(),
            Some(HistoryError::Snapshot(SnapshotError::NotFound(missing)))
        );
        assert!(matches!(
            view.tree(&StateRef::branch(name("nope")?)),
            Err(HistoryError::Branch(BranchError::NotFound(_)))
        ));
        Ok(())
    }

    fn agent_session(id: u64, actor: &Actor) -> SessionInfo {
        SessionInfo {
            id: SessionId(id),
            actor: actor.clone(),
            on_behalf_of: person(),
            transport: Transport::Stdio,
            client: "claude-code".to_owned(),
            model: None,
            parent: None,
            role: None,
            grants: Grants::agent_default(),
            fence: Vec::new(),
            budget: Budget::default(),
            spent: Spend::default(),
            limits: Limits::default(),
            state: SessionState::Active,
            open_tx: None,
            pending: Vec::new(),
            steps: 0,
            started_at: Seconds(0.0),
            last_active_at: Seconds(0.0),
        }
    }

    /// The agent sessions a test scripted for a lane; the rest of [`Sessions`] is as unused as in [`NoSessions`].
    struct Scripted(Mutex<Vec<SessionInfo>>);

    impl Scripted {
        fn with(sessions: Vec<SessionInfo>) -> Arc<Self> {
            Arc::new(Self(Mutex::new(sessions)))
        }

        fn end(&self, id: u64) {
            for session in lock(&self.0).iter_mut().filter(|s| s.id == SessionId(id)) {
                session.state = SessionState::Ended { reason: EndReason::Closed, at: Seconds(1.0) };
            }
        }
    }

    impl Sessions for Scripted {
        fn open(&self, spec: SessionSpec) -> Result<SessionInfo, OpError> {
            NoSessions.open(spec)
        }
        fn sessions(&self) -> Vec<SessionInfo> {
            lock(&self.0).clone()
        }
        fn session(&self, id: SessionId) -> Option<SessionInfo> {
            lock(&self.0).iter().find(|s| s.id == id).cloned()
        }
        fn session_of(&self, actor: &Actor) -> Option<SessionInfo> {
            let live = |s: &&SessionInfo| !matches!(s.state, SessionState::Ended { .. });
            lock(&self.0).iter().filter(live).find(|s| s.actor == *actor).cloned()
        }
        fn pause(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
            NoSessions.pause(id, by)
        }
        fn resume(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
            NoSessions.resume(id, by)
        }
        fn set_grants(&self, id: SessionId, grants: Grants, by: &Actor) -> Result<SessionInfo, OpError> {
            NoSessions.set_grants(id, grants, by)
        }
        fn set_budget(&self, id: SessionId, budget: Budget, by: &Actor) -> Result<SessionInfo, OpError> {
            NoSessions.set_budget(id, budget, by)
        }
        fn charge(&self, id: SessionId, spend: Spend) -> Result<SessionInfo, OpError> {
            NoSessions.charge(id, spend)
        }
        fn end(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError> {
            NoSessions.end(id, reason)
        }
        fn undo_session(&self, id: SessionId, by: &Actor) -> Result<SessionUndo, OpError> {
            NoSessions.undo_session(id, by)
        }
        fn approvals(&self) -> Vec<PendingApproval> {
            Vec::new()
        }
        fn resolve(&self, id: ApprovalId, decision: Decision, by: &Actor) -> Result<Resolution, OpError> {
            NoSessions.resolve(id, decision, by)
        }
    }

    /// Puts agent session 12 on main's lane, which is where the folder is.
    fn agent_on_main(fake: &FakeBranches) -> Arc<Scripted> {
        let sessions = Scripted::with(vec![agent_session(12, &agent())]);
        fake.script_lane(&BranchId::main(), Arc::new(NoRunner), sessions.clone());
        sessions
    }

    fn denied<T>(result: Result<T, BranchError>) -> bool {
        matches!(result, Err(BranchError::Denied(_)))
    }

    /// A document whose `merges` entity names these points of other branches (design 7.3).
    fn merged_doc(v: u64, merged: &[(&BranchId, u64, PointId)]) -> Value {
        let mut merges = serde_json::Map::new();
        merges.insert("format".to_owned(), json!(1));
        for (branch, seq, point) in merged {
            merges.insert(branch.to_string(), json!({"name": "merged", "seq": seq, "point": point.to_string()}));
        }
        json!({"v": v, "merges": merges})
    }

    /// Every version a branch or a point names is a real point of that branch, numbered as the version says.
    fn assert_versions_are_real(fake: &FakeBranches) -> Test {
        let store = fake.store();
        let check = |contains: &Contains| -> Test {
            for (branch, version) in contains.iter() {
                let point = store.point(&version.point)?;
                assert_eq!((&point.branch, point.seq), (branch, version.seq), "a version of {branch}");
            }
            Ok(())
        };
        for branch in fake.list() {
            check(&branch.inherits)?;
        }
        let contained: Vec<Contains> = lock(&store.state).points.values().map(|(p, _)| p.contains.clone()).collect();
        for contains in &contained {
            check(contains)?;
        }
        Ok(())
    }

    #[test]
    fn a_fork_point_belongs_to_its_source_however_the_branches_nest() -> Test {
        let (fake, _) = fake()?;
        let a = fork(&fake, "a", &person())?;
        assert_eq!(a.head, fake.store().this()?.head, "A starts at the point main already has");
        let b = fake.create(NewBranch::new(name("b")?, a.id.clone()), &person())?;
        let cut = fake.store().point(&b.head)?;
        assert_eq!((cut.branch.clone(), cut.seq), (a.id.clone(), 1), "a point of A, not one of main's");
        assert_eq!(b.inherits.get(&a.id), Some(&Version::new(1, b.head)));
        assert!(b.inherits.get(&BranchId::main()).is_some(), "and what A had inherited");

        fake.set_document(&a.id, json!({"v": "a"}));
        let first = fake.checkpoint(&a.id, "first", None, &person())?;
        let seq = fake.store().point(&first)?.seq;
        assert!(!b.inherits.includes(&a.id, &Version::new(seq, first)), "A's first checkpoint is not in B");

        let c = fake.create(NewBranch::new(name("c")?, b.id.clone()), &person())?;
        assert_eq!(fake.store().point(&c.head)?.branch, b.id, "three levels deep");
        assert_versions_are_real(&fake)
    }

    #[test]
    fn checkpoints_and_forks_carry_the_merges_a_document_records() -> Test {
        let (fake, _) = fake()?;
        let aria = fork(&fake, "aria", &person())?;
        fake.set_document(&aria.id, json!({"v": "oval"}));
        let oval = fake.checkpoint(&aria.id, "oval", None, &person())?;
        let seq = fake.store().point(&oval)?.seq;
        let version = Version::new(seq, oval);

        let before = fake.store().this()?.head;
        fake.set_document(&BranchId::main(), merged_doc(2, &[(&aria.id, seq, oval)]));
        let merge = fake.checkpoint(&BranchId::main(), "merge aria", None, &person())?;
        let point = fake.store().point(&merge)?;
        assert_eq!(point.contains.get(&aria.id), Some(&version));
        assert_eq!(point.parents, vec![before, oval], "the previous point and the point merged in");

        fake.set_document(&BranchId::main(), merged_doc(3, &[(&aria.id, seq, oval)]));
        let more = fake.checkpoint(&BranchId::main(), "more", None, &person())?;
        let later = fake.store().point(&more)?;
        assert_eq!(later.contains.get(&aria.id), Some(&version), "kept while the record is in the document");
        assert_eq!(later.parents, vec![merge], "and not listed as newly merged again");

        let cut = fork(&fake, "cut", &person())?;
        assert_eq!(cut.inherits.get(&aria.id), Some(&version), "a branch cut from here has the merge");

        // Undoing the merge undoes its record, and the next point forgets it.
        fake.set_document(&BranchId::main(), json!({"v": 4}));
        let undone = fake.checkpoint(&BranchId::main(), "undone", None, &person())?;
        assert_eq!(fake.store().point(&undone)?.contains.get(&aria.id), None);
        assert_versions_are_real(&fake)
    }

    #[test]
    fn the_merges_entity_reads_what_it_can_and_skips_the_rest() -> Test {
        let (own, other) = (BranchId::new("00000000000000aa")?, BranchId::new("00000000000000bb")?);
        let good = PointId(ContentHash::of(b"p")).to_string();
        let doc = json!({"merges": {
            "format": 1,
            "00000000000000aa": {"seq": 1, "point": good},
            "00000000000000bb": {"seq": 4, "point": good},
            "not-a-branch": {"seq": 2, "point": good},
            "00000000000000cc": {"seq": "four", "point": good},
            "00000000000000dd": {"seq": 3, "point": "xyz"},
        }});
        let point = PointId(ContentHash::of(b"p"));
        assert_eq!(merges_of(&doc, &own), Contains::new().with(other, Version::new(4, point)));
        assert_eq!(merges_of(&json!({"v": 1}), &own), Contains::new());
        assert_eq!(merges_of(&json!({"merges": "oops"}), &own), Contains::new());
        Ok(())
    }

    #[test]
    fn a_reference_to_the_branch_itself_is_its_live_document() -> Test {
        let (fake, _) = fake()?;
        let own = |text: &str, at: StateRef| -> Result<BranchInfo, Box<dyn Error>> {
            Ok(fake.create(NewBranch::new(name(text)?, BranchId::main()).at(at), &person())?)
        };
        fake.set_document(&BranchId::main(), json!({"v": "live 1"}));
        let by_name = own("by-name", StateRef::branch(BranchName::main()))?;
        assert_eq!(*fake.store().tree(&by_name.head)?, json!({"v": "live 1"}), "not main's older head");
        fake.set_document(&BranchId::main(), json!({"v": "live 2"}));
        let by_now = own("by-now", StateRef::Now)?;
        assert_eq!(*fake.store().tree(&by_now.head)?, json!({"v": "live 2"}));

        // Another branch's reference is still its head, not its live document.
        fake.set_document(&by_name.id, json!({"v": "not checkpointed"}));
        let other = own("from-other", StateRef::branch(name("by-name")?))?;
        assert_eq!(*fake.store().tree(&other.head)?, json!({"v": "live 1"}));

        let restore =
            fake.restore(&BranchId::main(), StateRef::branch(BranchName::main()), &person(), &Grants::person());
        assert!(matches!(restore, Err(BranchError::Invalid(_))), "restoring to itself changes nothing");
        assert_versions_are_real(&fake)
    }

    #[test]
    fn a_fork_from_now_and_a_restore_wait_for_an_open_transaction_and_change_nothing() -> Test {
        let (fake, _) = fake()?;
        let start = fake.store().this()?.head;
        fake.set_document(&BranchId::main(), json!({"v": "half done"}));
        fake.script_open_transaction(&BranchId::main(), true);
        let events = fake.subscribe();
        let seen = |fake: &FakeBranches| (fake.list(), fake.store().last_seq(&BranchId::main()));
        let before = seen(&fake);

        let from_now = NewBranch::new(name("x")?, BranchId::main());
        assert!(matches!(fake.create(from_now, &person()), Err(BranchError::Busy(_))));
        let restore = fake.restore(&BranchId::main(), StateRef::point(start), &person(), &Grants::person());
        assert!(matches!(restore, Err(BranchError::Busy(_))));
        assert_eq!(seen(&fake), before, "no point, branch or head was written");
        assert!(events.try_recv().is_err(), "and nothing was announced");

        // A fork from a recorded state captures no live state, so the open transaction does not hold it up.
        let from_point = NewBranch::new(name("old")?, BranchId::main()).at(StateRef::point(start));
        assert!(fake.create(from_point, &person()).is_ok());
        fake.script_open_transaction(&BranchId::main(), false);
        let x = fork(&fake, "x", &person())?;
        assert_eq!(x.id.as_str(), "0000000000000003", "the refused fork used up no id");
        assert_eq!(*fake.store().tree(&x.head)?, json!({"v": "half done"}));
        Ok(())
    }

    #[test]
    fn switching_leaves_a_lane_with_sessions_open_and_counts_them() -> Test {
        let (fake, _) = fake()?;
        let sessions = agent_on_main(&fake);
        let aria = fork(&fake, "aria", &person())?;
        assert_eq!(fake.list()[0].lane, LaneState::open(true, 1), "read from the lane's sessions");
        let events = fake.subscribe();
        fake.switch(&aria.id, &person())?;
        assert_eq!(fake.store().get(&BranchId::main()).map(|b| b.lane), Some(LaneState::open(false, 1)));
        let lanes: Vec<LaneState> = fake.list().into_iter().map(|b| b.lane).collect();
        assert_eq!(lanes, vec![LaneState::open(false, 1), LaneState::open(true, 0)]);
        let seen: Vec<BranchEvent> = events.try_iter().collect();
        assert_eq!(
            seen,
            vec![BranchEvent::lane_opened(aria.id.clone()), BranchEvent::switched(BranchId::main(), aria.id.clone())],
            "main's lane was not closed"
        );
        assert!(matches!(fake.delete(&BranchId::main(), &person()), Err(BranchError::Refused(_))));

        // Another session joins, and the first one ends: the count follows.
        let mut both =
            vec![agent_session(12, &agent()), agent_session(13, &Actor::Agent { id: "b".to_owned(), model: None })];
        both[0].state = SessionState::Ended { reason: EndReason::Closed, at: Seconds(1.0) };
        fake.script_lane(&BranchId::main(), Arc::new(NoRunner), Scripted::with(both));
        assert_eq!(fake.list()[0].lane, LaneState::open(false, 1), "the ended session is not counted");
        sessions.end(12);

        // A lane nobody works on closes when the folder leaves it.
        fake.switch(&BranchId::main(), &person())?;
        fake.script_lane(&aria.id, Arc::new(NoRunner), Arc::new(NoSessions));
        fake.script_lane(&BranchId::main(), Arc::new(NoRunner), Arc::new(NoSessions));
        fake.switch(&aria.id, &person())?;
        assert_eq!(fake.store().get(&BranchId::main()).map(|b| b.lane), Some(LaneState::Closed));
        Ok(())
    }

    #[test]
    fn an_agent_does_only_what_the_table_of_design_8_2_gives_it() -> Test {
        let (fake, _) = fake()?;
        let sessions = agent_on_main(&fake);
        let mine = fake.create(NewBranch::new(name("mine")?, BranchId::main()), &agent())?;
        assert_eq!(mine.owner_session, Some(SessionId(12)), "a branch an agent makes is made for its session");
        let theirs = fork(&fake, "theirs", &person())?;
        let other =
            fake.create(NewBranch::new(name("other")?, BranchId::main()).for_session(SessionId(99)), &person())?;

        // What it may do.
        fake.set_document(&mine.id, json!({"v": 1}));
        fake.checkpoint(&mine.id, "plain", None, &agent())?;
        fake.checkpoint(&theirs.id, "plain on another branch", None, &agent())?;
        fake.checkpoint(&mine.id, "ready", Some("note".to_owned()), &agent())?;
        fake.rename(&mine.id, name("mine2")?, None, &agent())?;
        fake.delete(&mine.id, &agent())?;
        fake.undelete(&mine.id, &agent())?;
        let own_session = NewBranch::new(name("y")?, BranchId::main()).for_session(SessionId(12));
        fake.create(own_session, &agent())?;

        // What it may not do.
        assert!(
            denied(fake.checkpoint(&theirs.id, "ready", Some(String::new()), &agent())),
            "review on another branch"
        );
        assert!(denied(fake.switch(&mine.id, &agent())));
        assert_eq!(fake.folder(), BranchId::main());
        assert!(denied(fake.review(&mine.id, ReviewDecision::MarkMerged, &agent())));
        assert!(denied(fake.review(&mine.id, ReviewDecision::send_back("no"), &agent())));
        let waiting = fake.store().get(&mine.id).and_then(|b| b.review).map(|r| r.state);
        assert_eq!(waiting, Some(ReviewState::Ready), "the person still decides");
        for branch in [&theirs.id, &other.id] {
            assert!(denied(fake.rename(branch, name("taken-over")?, None, &agent())));
            assert!(denied(fake.delete(branch, &agent())));
        }
        fake.delete(&theirs.id, &person())?;
        assert!(denied(fake.undelete(&theirs.id, &agent())));
        let for_another = NewBranch::new(name("z")?, BranchId::main()).for_session(SessionId(99));
        assert!(denied(fake.create(for_another, &agent())));
        let plugin = Actor::Plugin { id: "grain".to_owned() };
        assert!(denied(fake.switch(&mine.id, &plugin)));
        assert!(denied(fake.review(&mine.id, ReviewDecision::MarkMerged, &plugin)));

        // A person does all of it; an agent whose session ended owns nothing.
        fake.rename(&other.id, name("other2")?, None, &person())?;
        fake.review(&mine.id, ReviewDecision::MarkMerged, &person())?;
        sessions.end(12);
        assert!(denied(fake.rename(&mine.id, name("mine3")?, None, &agent())));
        Ok(())
    }

    #[test]
    fn a_restore_needs_the_edit_scopes_of_what_it_changes_and_a_refusal_changes_nothing() -> Test {
        let (fake, _) = fake()?;
        let start = fake.store().this()?.head;
        let live = json!({"face": {"jaw": 0.9}, "characters": {"aria": {"v": 1}}});
        fake.set_document(&BranchId::main(), live.clone());
        let seen = |fake: &FakeBranches| (fake.list(), fake.store().last_seq(&BranchId::main()));
        let before = seen(&fake);
        let to_start = StateRef::point(start);

        let read_only = Grants::of([Scope::Read]);
        let result = fake.restore(&BranchId::main(), to_start.clone(), &person(), &read_only);
        assert!(matches!(&result, Err(BranchError::Denied(why)) if why.contains("edit:character")), "{result:?}");
        let characters = Grants::of([Scope::Read, Scope::EditCharacter]);
        let result = fake.restore(&BranchId::main(), to_start, &person(), &characters);
        assert!(matches!(&result, Err(BranchError::Denied(why)) if why.contains("edit:scene")), "{result:?}");
        assert_eq!(seen(&fake), before, "the refused restores wrote nothing");
        let kept = fake.checkpoint(&BranchId::main(), "still here", None, &person())?;
        assert_eq!(*fake.store().tree(&kept)?, live, "and the live document is as it was");

        // A change inside `characters` needs only `edit:character`.
        fake.set_document(&BranchId::main(), json!({"face": {"jaw": 0.9}, "characters": {"aria": {"v": 2}}}));
        let restored = fake.restore(&BranchId::main(), StateRef::point(kept), &person(), &characters)?;
        assert_eq!(
            *fake.store().tree(&restored.backup.head)?,
            json!({"face": {"jaw": 0.9}, "characters": {"aria": {"v": 2}}})
        );
        assert_versions_are_real(&fake)
    }

    #[test]
    fn stamps_are_utc_dates_and_times() {
        assert_eq!(stamp(WallTime(0)), ("1970-01-01-0000".to_owned(), "00:00".to_owned()));
        assert_eq!(stamp(WallTime(1_760_279_520_000)).0, "2025-10-12-1432");
        assert_eq!(stamp(WallTime(951_782_400_000 + 23 * 3_600_000 + 59 * 60_000)).0, "2000-02-29-2359", "a leap day");
    }
}
