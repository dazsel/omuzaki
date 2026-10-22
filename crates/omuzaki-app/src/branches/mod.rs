//! Branches in the app core (`docs/design/branches.md` sections 5, 8 and 9; ADR 0025): [`HostBranches`], the lane
//! manager. It keeps one lane per open branch (an executor, a journal on the branch's root and a keeper, see
//! [`OpenLane`]), makes points of a lane's committed state, creates, renames, deletes and reviews branches, switches
//! the folder between lanes crash-safely, and keeps the abandoned future of a restore on an automatic branch.
//!
//! **Locks.** The manager holds its own lock while it calls into a lane, never the other way round: ops run under
//! their lane's lock and read other branches only through points (immutable objects in the store) and the lane
//! states, a leaf lock nothing else is taken under. So no lane waits on another, and no op waits on the manager.
//!
//! **Who may do what** (design 8.2): a person may do everything; an agent may create branches for its own session,
//! checkpoint, ask for review on a branch it works on, and rename or delete only a branch made for its session.
//! Switching the folder and deciding a review are a person's.

mod lane;
pub mod merges;
pub mod ops;
pub mod routed;

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use omuzaki_doc::branch::{
    DiskBranchStore, FolderMarker, FolderState, branch_root, new_branch_id, read_point, write_point,
};
use omuzaki_doc::journal::{JournalError, Recover, Reopened, left_open_in, reopen_on, reopen_side};
use omuzaki_doc_api::Value;
use omuzaki_history_api::branch::{
    BranchError, BranchEvent, BranchId, BranchInfo, BranchName, Branches, Contains, Lane, LaneState, NewBranch, Point,
    PointId, PointKind, Restored, Review, ReviewDecision, ReviewState, Switched, Version,
};
use omuzaki_history_api::state::{BranchStore, HistoryView, StateRef};
use omuzaki_history_api::{HistoryExport, WallTime};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, EndReason, Event, Grants, Jobs, Mode, OpError, Request, Runner, SessionId, SessionState, Sessions,
};
use serde_json::json;

pub use lane::OpenLane;
pub(crate) use lane::{LaneStates, Shared};

use crate::AppError;
use crate::recover::RecoveryReport;

/// How long a lane step (a point's snapshot, a save, a folder rewrite) may take.
const LANE_WAIT: Duration = Duration::from_secs(30);

/// A side lane closes this long after it last had a session, a handle or the folder (design 5.2).
pub const IDLE_CLOSE: Duration = Duration::from_secs(60);

/// A hook called at each step of a switch (`switch:marked`, `switch:old-point`, `switch:rewritten`): an error stops
/// the switch there, as a crash would. Tests set it; it is a no-op otherwise.
pub type Failpoint = Arc<dyn Fn(&'static str) -> Result<(), String> + Send + Sync>;

/// What made a point.
struct Made<'a> {
    kind: PointKind,
    label: &'a str,
    by: &'a Actor,
    /// The branch's head moves to it. A point of an older state doesn't move the head back.
    advance: bool,
}

/// The lane manager of one open project. See the [module docs](self).
pub struct HostBranches {
    shared: Shared,
    /// Reads refs and points for the manager itself.
    store: DiskBranchStore,
    state: Mutex<State>,
    subscribers: Mutex<Vec<Sender<BranchEvent>>>,
    max_lanes: usize,
    failpoint: Failpoint,
    me: Weak<HostBranches>,
}

struct State {
    lanes: BTreeMap<BranchId, Arc<OpenLane>>,
    folder: BranchId,
    /// What recovering a side lane at open found, by branch, for the branch menu's notice.
    recovered: Vec<(BranchId, String)>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn io(error: impl std::fmt::Display) -> BranchError {
    BranchError::Io(error.to_string())
}

/// Whether `by` is a person (or Omuzaki itself), who may do everything design 8.2 lists.
fn is_person(by: &Actor) -> bool {
    matches!(by, Actor::Person { .. } | Actor::System)
}

fn person_only(by: &Actor, what: &str) -> Result<(), BranchError> {
    if is_person(by) { Ok(()) } else { Err(BranchError::Denied(format!("{what} is for a person: ask the person"))) }
}

/// `yyyy-mm-dd-hhmm` and `hh:mm` of a UTC time in milliseconds, for the names of automatic branches.
fn stamp(time: WallTime) -> (String, String) {
    let minutes = time.0 / 60_000;
    let (days, minute_of_day) = (minutes / 1440, minutes % 1440);
    // The civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
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

impl From<AppError> for BranchError {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Branch(branch) => branch,
            other => BranchError::Io(other.to_string()),
        }
    }
}

impl HostBranches {
    /// Opens the folder lane of the project `shared` describes: the branch `folder.toml` names, exactly as ADR 0021
    /// opens a project (with `mode` for a crash), or, when a switch was cut short, the target branch from its own
    /// journal with the folder rewritten to match (design 5.5). Then every other branch whose lane stopped without
    /// closing is recovered by the headless rules and closed again; [`recovered`](Self::recovered) says what each
    /// found.
    ///
    /// # Errors
    /// As opening the lane; [`AppError::Branch`] when the marker or a ref can't be read.
    pub(crate) fn open(
        shared: Shared,
        mode: Recover,
        failpoint: Failpoint,
        max_lanes: usize,
    ) -> Result<Arc<Self>, AppError> {
        let dir = shared.dir.clone();
        let marker = FolderMarker::read(&dir)?;
        let folder = marker.target().clone();
        let store = DiskBranchStore::new(BranchId::main(), shared.refs.clone(), shared.snapshots.clone());
        let lane = if marker.state == FolderState::Switching {
            // The folder was being rewritten when Omuzaki stopped: the target's own journal is the truth (without the
            // folder prompt, since the folder was being replaced), and steps 5 and 6 are redone.
            let reopened = side_reopen(&shared, &store, &folder)?;
            let lane = lane::open(&shared, &folder, reopened, false, true)?;
            lane.journal.set_folder(true, LANE_WAIT)?;
            FolderMarker::finish_switch(&dir, &folder)?;
            lane
        } else {
            let reopened = reopen_on(&dir, &shared.snapshots, mode, &folder)?;
            lane::open(&shared, &folder, reopened, true, true)?
        };
        let lane = Arc::new(lane);
        let this = Arc::new_cyclic(|me| Self {
            shared,
            store,
            state: Mutex::new(State {
                lanes: BTreeMap::from([(folder.clone(), Arc::clone(&lane))]),
                folder,
                recovered: Vec::new(),
            }),
            subscribers: Mutex::new(Vec::new()),
            max_lanes,
            failpoint,
            me: me.clone(),
        });
        this.watch(&lane);
        {
            let mut state = lock(&this.state);
            this.sync_states(&state);
            this.recover_side_lanes(&mut state);
        }
        Ok(this)
    }

    /// The folder lane: what the person's window, Ctrl+Z and the files address.
    pub fn folder_lane(&self) -> Arc<OpenLane> {
        let state = lock(&self.state);
        // The folder lane is open from project open to close; only a switch replaces it, under this lock.
        let folder = state.lanes.get(&state.folder).or_else(|| state.lanes.values().next());
        folder.map(Arc::clone).unwrap_or_else(|| unreachable_lane())
    }

    /// The lane of `branch` if it is open.
    pub fn open_lane(&self, branch: &BranchId) -> Option<Arc<OpenLane>> {
        lock(&self.state).lanes.get(branch).map(Arc::clone)
    }

    /// The lane `actor`'s session works on: an agent's session belongs to one lane; anyone else is on the folder's.
    pub fn lane_of_actor(&self, actor: &Actor) -> Arc<OpenLane> {
        if matches!(actor, Actor::Agent { .. }) {
            let state = lock(&self.state);
            if let Some(lane) = state.lanes.values().find(|lane| lane.executor.session_of(actor).is_some()) {
                return Arc::clone(lane);
            }
        }
        self.folder_lane()
    }

    /// Every open lane, the folder's first.
    pub fn lanes(&self) -> Vec<Arc<OpenLane>> {
        let state = lock(&self.state);
        let mut lanes: Vec<Arc<OpenLane>> = state.lanes.values().map(Arc::clone).collect();
        lanes.sort_by_key(|lane| lane.branch != state.folder);
        lanes
    }

    /// What recovering side lanes at open found: each branch with its notice.
    pub fn recovered(&self) -> Vec<(BranchId, String)> {
        lock(&self.state).recovered.clone()
    }

    /// The live branch called `name`.
    ///
    /// # Errors
    /// [`BranchError::NotFound`].
    pub fn by_name(&self, name: &BranchName) -> Result<BranchInfo, BranchError> {
        self.store.branch(name)
    }

    /// The branch store every lane's ops read, for the host's own reads.
    pub fn store(&self) -> &DiskBranchStore {
        &self.store
    }

    /// Closes every side lane that has been idle (no session, no handle, not the folder) for at least `idle`. The
    /// host calls it about every few seconds with [`IDLE_CLOSE`]; a test calls it with what it needs.
    pub fn close_idle(&self, idle: Duration) {
        let mut state = lock(&self.state);
        let idle_lanes: Vec<BranchId> = state
            .lanes
            .values()
            .filter(|lane| {
                let busy = lane.branch == state.folder || lane.sessions() > 0 || lane.held();
                lane.idle_for(busy) >= idle && !busy
            })
            .map(|lane| lane.branch.clone())
            .collect();
        for id in idle_lanes {
            if let Err(error) = self.close_lane(&mut state, &id) {
                tracing::warn!(branch = %id, %error, "an idle branch didn't close cleanly; it recovers at its next open");
            }
        }
        self.sync_states(&state);
    }

    /// Closes every lane: side lanes make a close point; the folder lane saves the files and, once the project has
    /// branched, makes one too, so others merge what it ended with. The journals end with their `close` records.
    ///
    /// # Errors
    /// The first lane that didn't close; the others are closed all the same.
    pub fn close_all(&self) -> Result<(), AppError> {
        let mut state = lock(&self.state);
        let mut first = None;
        let mut ids: Vec<BranchId> = state.lanes.keys().cloned().collect();
        // The folder lane last, so its files are the last thing written.
        ids.sort_by_key(|id| *id == state.folder);
        for id in ids {
            if let Err(error) = self.close_lane(&mut state, &id) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), |error| Err(AppError::Branch(error)))
    }

    // ----- inside the manager's lock -----

    fn emit(&self, event: &BranchEvent) {
        lock(&self.subscribers).retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Publishes what each lane is doing, for the branch stores' `lane` field.
    fn sync_states(&self, state: &State) {
        let states: BTreeMap<BranchId, LaneState> = state
            .lanes
            .values()
            .map(|lane| (lane.branch.clone(), LaneState::open(lane.branch == state.folder, lane.sessions())))
            .collect();
        *self.shared.lane_states.write().unwrap_or_else(PoisonError::into_inner) = states;
    }

    fn filled(&self, mut info: BranchInfo) -> BranchInfo {
        let states = self.shared.lane_states.read().unwrap_or_else(PoisonError::into_inner);
        info.lane = states.get(&info.id).copied().unwrap_or(LaneState::Closed);
        info
    }

    fn live(&self, id: &BranchId) -> Result<BranchInfo, BranchError> {
        match self.shared.refs.read(id)? {
            Some(info) if info.deleted.is_none() => Ok(info),
            _ => Err(BranchError::NotFound(id.to_string())),
        }
    }

    /// Watches `lane`'s applied ops, so a merge of a reviewed point marks its review merged (design 8.3).
    fn watch(&self, lane: &OpenLane) {
        let Ok(events) = lane.executor.subscribe(&Grants::person()) else { return };
        let me = self.me.clone();
        let spawned = std::thread::Builder::new().name("omuzaki-branch-watch".to_owned()).spawn(move || {
            while let Ok(event) = events.recv() {
                let Event::OpApplied { record, .. } = &event else { continue };
                if record.op != ops::MERGE_OP {
                    continue;
                }
                let Some(me) = me.upgrade() else { return };
                me.merged(&record.args);
            }
        });
        if let Err(error) = spawned {
            tracing::warn!(%error, "reviews won't be marked merged automatically on this branch");
        }
    }

    /// A `branch.merge` with `args` applied: the review of the point it merged, if one waits, is merged.
    fn merged(&self, args: &Value) {
        let Some(from) = args.get("from").and_then(Value::as_str).and_then(|n| n.parse::<BranchName>().ok()) else {
            return;
        };
        let Ok(info) = self.store.branch(&from) else { return };
        let at = args.get("at").and_then(Value::as_str).and_then(|p| p.parse::<PointId>().ok()).unwrap_or(info.head);
        let waiting = info.review.as_ref().is_some_and(|r| r.state == ReviewState::Ready && r.point == at);
        if !waiting {
            return;
        }
        let updated = self.shared.refs.update(&info.id, self.shared.snapshots.store().as_ref(), |info| {
            if let Some(review) = &mut info.review {
                review.state = ReviewState::Merged;
            }
            Ok(())
        });
        match updated {
            Ok(_) => self.emit(&BranchEvent::reviewed(info.id, ReviewState::Merged)),
            Err(error) => tracing::warn!(%error, "a merged review couldn't be recorded"),
        }
    }

    fn recover_side_lanes(&self, state: &mut State) {
        let folder = state.folder.clone();
        let refs = self.shared.refs.list().unwrap_or_default();
        for info in refs.into_iter().filter(|i| i.deleted.is_none() && i.id != folder) {
            if !left_open_in(&branch_root(&self.shared.dir, &info.id)) {
                continue;
            }
            match self.open_side(state, &info.id) {
                Ok(lane) => {
                    let notice = recovered_notice(&info, &lane.recovery);
                    if let Err(error) = self.close_lane(state, &info.id) {
                        tracing::warn!(branch = %info.name, %error, "a recovered branch didn't close");
                    }
                    state.recovered.push((info.id.clone(), notice.clone()));
                    self.emit(&BranchEvent::lane_recovered(info.id, notice));
                }
                Err(error) => {
                    let notice = format!("branch {} couldn't be recovered: {error}", info.name);
                    state.recovered.push((info.id.clone(), notice.clone()));
                    self.emit(&BranchEvent::lane_recovered(info.id, notice));
                }
            }
        }
    }

    /// `id`'s lane, opened as a side lane if it is closed. Opening a ninth closes the longest-idle side lane with
    /// nothing on it first, or is refused.
    fn lane_locked(&self, state: &mut State, id: &BranchId) -> Result<Arc<OpenLane>, BranchError> {
        if let Some(lane) = state.lanes.get(id) {
            lane.idle_for(true);
            return Ok(Arc::clone(lane));
        }
        self.open_side(state, id)
    }

    fn open_side(&self, state: &mut State, id: &BranchId) -> Result<Arc<OpenLane>, BranchError> {
        if state.lanes.len() >= self.max_lanes {
            let idle = state
                .lanes
                .values()
                .filter(|lane| lane.branch != state.folder && lane.sessions() == 0 && !lane.held())
                .max_by_key(|lane| lane.idle_for(false))
                .map(|lane| lane.branch.clone());
            let Some(idle) = idle else { return Err(BranchError::LaneLimit) };
            self.close_lane(state, &idle)?;
        }
        let reopened = side_reopen(&self.shared, &self.store, id)?;
        let lane = Arc::new(lane::open(&self.shared, id, reopened, false, false)?);
        state.lanes.insert(id.clone(), Arc::clone(&lane));
        self.watch(&lane);
        self.sync_states(state);
        self.emit(&BranchEvent::lane_opened(id.clone()));
        if !lane.recovery.is_empty() {
            let name = self.live(id).map(|i| i.name.to_string()).unwrap_or_else(|_| id.to_string());
            self.emit(&BranchEvent::lane_recovered(id.clone(), format!("{name}: {}", lane.recovery.summary())));
        }
        Ok(lane)
    }

    /// Closes `id`'s lane (design 5.2): its sessions end (their open transactions roll back), its jobs stop, a close
    /// point records what it ended with (when the branch has a ref and the state changed), and its journal closes.
    fn close_lane(&self, state: &mut State, id: &BranchId) -> Result<(), BranchError> {
        let Some(lane) = state.lanes.get(id).map(Arc::clone) else { return Ok(()) };
        for session in Sessions::sessions(lane.executor.as_ref()) {
            if !matches!(session.state, SessionState::Ended { .. }) {
                drop(lane.executor.end(session.id, EndReason::Closed));
            }
        }
        lane.executor.shutdown_jobs();
        let point = match self.shared.refs.read(id)? {
            Some(info) if info.deleted.is_none() => {
                let made = Made { kind: PointKind::Close, label: "Closed", by: &Actor::System, advance: true };
                self.point_if_changed(&lane, &info, &made).map(drop)
            }
            _ => Ok(()),
        };
        let closed = lane.journal.close(lane.executor.history_export(), LANE_WAIT).map(drop).map_err(io);
        state.lanes.remove(id);
        self.sync_states(state);
        self.emit(&BranchEvent::lane_closed(id.clone()));
        point.and(closed)
    }

    /// `main`'s ref, made the first time anything branches (design 4.1): a point of the folder or main lane's state.
    fn ensure_main(&self, state: &mut State) -> Result<BranchInfo, BranchError> {
        if let Some(info) = self.shared.refs.read(&BranchId::main())? {
            return Ok(info);
        }
        let lane = self.lane_locked(state, &BranchId::main())?;
        let committed = lane.executor.committed().map_err(|e| BranchError::Busy(e.message))?;
        let snapshot = lane
            .journal
            .take_point(
                Some(committed.tree),
                "main",
                committed.history.clone().unwrap_or_else(HistoryExport::empty),
                LANE_WAIT,
            )
            .map_err(io)?;
        let now = self.shared.wall.now();
        let mut point =
            Point::new(BranchId::main(), 1, snapshot.id, snapshot.lsn, PointKind::Checkpoint, Actor::System, now)
                .with_label("main");
        if let (Some(_), Some(history)) = (&committed.history, snapshot.history) {
            point = point.with_history(history);
        }
        let store = self.shared.snapshots.store().as_ref();
        let id = write_point(store, &point)?;
        let mut info = BranchInfo::new(BranchId::main(), BranchName::main(), id, 1, now, Actor::System);
        info.last_seq = 1;
        self.shared.refs.create(&info, store)?;
        Ok(info)
    }

    /// A point of `lane`'s committed state (or of `tree`, a state from its history) on `info`'s branch (design 5.3):
    /// the snapshot is the keeper's, the point names what the state includes (its `merges` record and the branch's
    /// `inherits`), its parents are the branch's previous point and the points newly merged since, and it carries the
    /// history only when no transaction was open.
    fn record(
        &self,
        lane: &OpenLane,
        info: &BranchInfo,
        made: &Made<'_>,
        tree: Option<Value>,
    ) -> Result<(Point, PointId), BranchError> {
        let (tree, history) = match tree {
            Some(tree) => (tree, None),
            None => {
                let committed = lane.executor.committed().map_err(|e| BranchError::Busy(e.message))?;
                (committed.tree, committed.history)
            }
        };
        let store = self.shared.snapshots.store().as_ref();
        let head = read_point(store, &info.head)?;
        let snapshot = lane
            .journal
            .take_point(Some(tree.clone()), made.label, history.clone().unwrap_or_else(HistoryExport::empty), LANE_WAIT)
            .map_err(io)?;
        let merged = merges::verified_merges(&tree, &info.id, &self.store);
        let mut parents = vec![info.head];
        parents.extend(merged.iter().filter(|(b, v)| !head.contains.includes(b, v)).map(|(_, v)| v.point));
        let own = if head.branch == info.id { info.seq } else { 0 };
        let seq = own.max(info.last_seq) + 1;
        let mut point = Point::new(
            info.id.clone(),
            seq,
            snapshot.id,
            snapshot.lsn,
            made.kind,
            made.by.clone(),
            self.shared.wall.now(),
        )
        .with_parents(parents)
        .with_contains(merged.join(&info.inherits))
        .with_label(made.label);
        if let (Some(_), Some(blob)) = (&history, snapshot.history) {
            point = point.with_history(blob);
        }
        let id = write_point(store, &point)?;
        let advance = made.advance;
        self.shared.refs.update(&info.id, store, |info| {
            info.last_seq = info.last_seq.max(seq);
            if advance {
                info.head = id;
                info.seq = seq;
            }
            Ok(())
        })?;
        Ok((point, id))
    }

    /// [`record`](Self::record), unless the committed state is the head's own state: then the head.
    fn point_if_changed(
        &self,
        lane: &OpenLane,
        info: &BranchInfo,
        made: &Made<'_>,
    ) -> Result<(Point, PointId), BranchError> {
        let committed = lane.executor.committed().map_err(|e| BranchError::Busy(e.message))?;
        let head = self.store.point(&info.head)?;
        if head.branch == info.id && *self.store.tree(&info.head)? == committed.tree {
            return Ok((head, info.head));
        }
        self.record(lane, info, made, None)
    }

    /// The session `by` runs, in any lane.
    fn session_of(state: &State, by: &Actor) -> Option<(SessionId, BranchId)> {
        state.lanes.values().find_map(|lane| lane.executor.session_of(by).map(|s| (s.id, lane.branch.clone())))
    }

    fn made_for(state: &State, by: &Actor, branch: &BranchInfo) -> bool {
        branch.owner_session.is_some() && branch.owner_session == Self::session_of(state, by).map(|(id, _)| id)
    }

    fn works_on(state: &State, by: &Actor, branch: &BranchInfo) -> bool {
        Self::made_for(state, by, branch) || Self::session_of(state, by).is_some_and(|(_, lane)| lane == branch.id)
    }

    fn may_manage(state: &State, by: &Actor, branch: &BranchInfo, what: &str) -> Result<(), BranchError> {
        if is_person(by) || (matches!(by, Actor::Agent { .. }) && Self::made_for(state, by, branch)) {
            return Ok(());
        }
        Err(BranchError::Denied(format!("an agent may {what} only a branch made for its session, not {}", branch.name)))
    }

    fn new_branch_owner(state: &State, by: &Actor, asked: Option<SessionId>) -> Result<Option<SessionId>, BranchError> {
        if is_person(by) {
            return Ok(asked);
        }
        match (asked, Self::session_of(state, by).map(|(id, _)| id)) {
            (_, None) => Err(BranchError::Denied(
                "an agent makes a branch for its own session, and no session of yours runs here".to_owned(),
            )),
            (Some(asked), Some(own)) if asked != own => {
                Err(BranchError::Denied("an agent makes a branch only for its own session".to_owned()))
            }
            (asked, Some(own)) => Ok(Some(asked.unwrap_or(own))),
        }
    }

    /// Whether `point` is part of `source`'s history: its own, or one its state includes.
    fn in_history(&self, source: &BranchInfo, point: &Point) -> Result<bool, BranchError> {
        if point.branch == source.id {
            return Ok(true);
        }
        let included = self.store.point(&source.head)?.contains.join(&source.inherits);
        Ok(included.includes(&point.branch, &Version::new(point.seq, point.id()?)))
    }

    /// The fork point of `source` at `at` (design 5.4): from now, a checkpoint (or the head when nothing changed);
    /// from a state of its history, a point made from that tree, which doesn't move the source's head.
    fn fork_point(
        &self,
        state: &mut State,
        source: &BranchInfo,
        at: &StateRef,
        new_name: &BranchName,
        by: &Actor,
    ) -> Result<(Point, PointId), BranchError> {
        let elsewhere = |what: String| {
            BranchError::Invalid(format!(
                "{what} is not a state of branch {}: fork or restore that branch's own states",
                source.name
            ))
        };
        let label = format!("Fork {new_name}");
        let lane = self.lane_locked(state, &source.id)?;
        let made = Made { kind: PointKind::Fork, label: &label, by, advance: true };
        match at {
            StateRef::Now => self.point_if_changed(&lane, source, &made),
            StateRef::Point { id, .. } => {
                let point = self.store.point(id)?;
                if !self.in_history(source, &point)? {
                    return Err(elsewhere(format!("point {id}")));
                }
                if point.branch == source.id {
                    return Ok((point, *id));
                }
                let tree = self.store.tree(id)?.as_ref().clone();
                self.record(&lane, source, &Made { advance: false, ..made }, Some(tree))
            }
            StateRef::Tx { .. } => {
                let tree = lane.history.tree(at).map_err(|e| BranchError::Invalid(e.to_string()))?;
                self.record(&lane, source, &Made { advance: false, ..made }, Some(tree.as_ref().clone()))
            }
            StateRef::Snapshot { id, .. } => {
                let info = self.shared.snapshots.refs().map_err(io)?.into_iter().find(|info| info.id == *id);
                match info {
                    Some(info) if info.branch == source.id => {}
                    Some(_) => return Err(elsewhere(format!("snapshot {id}"))),
                    None => return Err(BranchError::Invalid(format!("no snapshot {id}"))),
                }
                let tree = self.shared.snapshots.load(id).map_err(io)?.root().clone();
                self.record(&lane, source, &Made { advance: false, ..made }, Some(tree))
            }
            StateRef::Branch { name, .. } => Err(elsewhere(format!("branch {name}"))),
            _ => Err(BranchError::Invalid("this kind of state reference is not known here".to_owned())),
        }
    }

    /// `at` as `source` reads it: a reference to `source` itself by name is its live document, `now`.
    fn normalized(&self, source: &BranchInfo, at: &StateRef) -> StateRef {
        match at {
            StateRef::Branch { name, .. } if self.store.branch(name).is_ok_and(|b| b.id == source.id) => StateRef::Now,
            other => other.clone(),
        }
    }

    /// A free automatic name `prefix/slug`, `-2` and on when taken.
    fn free_name(&self, base: &str) -> Result<BranchName, BranchError> {
        let mut name = base.to_owned();
        let mut suffix = 1;
        while self.store.branch(&BranchName::new(name.clone())?).is_ok() {
            suffix += 1;
            name = format!("{base}-{suffix}");
        }
        Ok(BranchName::new(name)?)
    }

    /// The new branch's ref, forked at `fork`.
    fn new_ref(
        &self,
        name: BranchName,
        source: &BranchId,
        fork: (&Point, PointId),
        by: &Actor,
    ) -> Result<BranchInfo, BranchError> {
        let (point, id) = fork;
        let mut info =
            BranchInfo::new(new_branch_id().map_err(io)?, name, id, point.seq, self.shared.wall.now(), by.clone());
        info.from = Some(id);
        info.inherits = point.contains.join(&Contains::new().with(source.clone(), Version::new(point.seq, id)));
        Ok(info)
    }
}

/// Never reached: the folder lane is open for as long as the manager is.
#[allow(clippy::panic, reason = "an invariant of the manager: the folder lane is always open")]
fn unreachable_lane() -> Arc<OpenLane> {
    panic!("the folder lane is always open while the project is")
}

fn recovered_notice(info: &BranchInfo, report: &RecoveryReport) -> String {
    let what = if report.is_empty() { "closed cleanly".to_owned() } else { report.summary() };
    format!("branch {} was recovered after Omuzaki stopped: {what}", info.name)
}

/// What reopening `branch`'s side lane found: its own journal and snapshots, or its head point when its lane never
/// ran (the fork point's state, with the source's undo history when the fork had one).
fn side_reopen(shared: &Shared, store: &DiskBranchStore, branch: &BranchId) -> Result<Reopened, AppError> {
    let info = shared.refs.read(branch)?.ok_or_else(|| BranchError::NotFound(branch.to_string()))?;
    let snapshots = &shared.snapshots;
    let head = || -> Result<_, JournalError> {
        let point = store.point(&info.head).map_err(|e| JournalError::Snapshot(e.to_string()))?;
        let project = snapshots.load(&point.snapshot).map_err(|e| JournalError::Snapshot(e.to_string()))?;
        let history = match point.history {
            Some(blob) => {
                let bytes = snapshots.store().get(&blob).map_err(|e| JournalError::Snapshot(e.to_string()))?;
                serde_json::from_slice(&bytes).map_err(|e| JournalError::Snapshot(e.to_string()))?
            }
            None => HistoryExport::empty(),
        };
        Ok((project, history))
    };
    Ok(reopen_side(&shared.dir, snapshots, branch, head)?)
}

impl Branches for HostBranches {
    fn list(&self) -> Vec<BranchInfo> {
        self.shared.refs.list().unwrap_or_default().into_iter().map(|info| self.filled(info)).collect()
    }

    fn folder(&self) -> BranchId {
        lock(&self.state).folder.clone()
    }

    fn create(&self, new: NewBranch, by: &Actor) -> Result<BranchInfo, BranchError> {
        let mut state = lock(&self.state);
        self.ensure_main(&mut state)?;
        let source = self.live(&new.from_branch)?;
        let owner = Self::new_branch_owner(&state, by, new.for_session)?;
        if self.store.branch(&new.name).is_ok() {
            return Err(BranchError::NameTaken(new.name));
        }
        let at = self.normalized(&source, &new.at);
        let (fork, fork_id) = self.fork_point(&mut state, &source, &at, &new.name, by)?;
        let mut info = self.new_ref(new.name, &source.id, (&fork, fork_id), by)?;
        info.title = new.title;
        info.focus = new.focus;
        info.owner_session = owner;
        self.shared.refs.create(&info, self.shared.snapshots.store().as_ref())?;
        let info = self.filled(info);
        self.emit(&BranchEvent::created(info.clone()));
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
        self.ensure_main(&mut state)?;
        let info = self.live(branch)?;
        if ready.is_some() && !is_person(by) && !Self::works_on(&state, by, &info) {
            return Err(BranchError::Denied(format!(
                "an agent asks for review only on a branch it works on, not {}",
                info.name
            )));
        }
        let lane = self.lane_locked(&mut state, branch)?;
        let is_ready = ready.is_some();
        let kind = if is_ready { PointKind::Ready } else { PointKind::Checkpoint };
        let made = Made { kind, label, by, advance: true };
        let (_, id) = if is_ready {
            self.record(&lane, &info, &made, None)?
        } else {
            self.point_if_changed(&lane, &info, &made)?
        };
        if id == info.head && !is_ready {
            return Ok(id);
        }
        if is_ready {
            let review = Review::new(id, by.clone(), self.shared.wall.now(), ready);
            self.shared.refs.update(branch, self.shared.snapshots.store().as_ref(), |info| {
                info.review = Some(review);
                Ok(())
            })?;
        }
        self.emit(&BranchEvent::checkpointed(branch.clone(), id, is_ready));
        if is_ready {
            self.emit(&BranchEvent::review_requested(branch.clone(), id));
        }
        Ok(id)
    }

    fn switch(&self, to: &BranchId, by: &Actor) -> Result<Switched, BranchError> {
        if !is_person(by) {
            return Err(BranchError::Denied(
                "switching the folder is for a person: use branch_work to move your session".to_owned(),
            ));
        }
        let mut state = lock(&self.state);
        self.ensure_main(&mut state)?;
        let target = self.live(to)?;
        self.store.point(&target.head)?;
        let from = state.folder.clone();
        if from == *to {
            return Ok(Switched::new(from, to.clone()));
        }
        let old_info = self.live(&from)?;
        let old = self.lane_locked(&mut state, &from)?;
        // 1. Everything that can refuse comes first, so a refused switch changes nothing.
        let export = old.executor.history_export();
        if let Some(open) = export.open.iter().find(|tx| matches!(tx.owner, Actor::Person { .. })) {
            return Err(BranchError::Busy(format!("finish or cancel '{}' first", open.label)));
        }
        let outside = old.journal.outside_changes(LANE_WAIT).map_err(io)?;
        if !outside.files.is_empty() {
            return Err(BranchError::Busy(format!(
                "{} changed outside Omuzaki: reload or discard those changes first, or switching overwrites them",
                outside.files.join(", ")
            )));
        }
        old.executor.committed().map_err(|e| BranchError::Busy(e.message))?;
        // 2. The target's lane.
        let target_lane = self.lane_locked(&mut state, to)?;
        let fail = |step: &'static str| (self.failpoint)(step).map_err(BranchError::Io);
        // 3. The marker, flushed.
        FolderMarker::begin_switch(&self.shared.dir, &from, to)?;
        fail("switch:marked")?;
        // 4. The old folder lane stops saving and records where it was.
        old.journal.set_folder(false, LANE_WAIT).map_err(io)?;
        let made = Made { kind: PointKind::Switch, label: "Switched away", by, advance: true };
        self.point_if_changed(&old, &old_info, &made)?;
        fail("switch:old-point")?;
        // 5. The target's keeper rewrites the folder exactly and saves from there.
        target_lane.journal.set_folder(true, LANE_WAIT).map_err(io)?;
        fail("switch:rewritten")?;
        // 6. Done.
        FolderMarker::finish_switch(&self.shared.dir, to)?;
        state.folder = to.clone();
        if old.sessions() == 0 {
            self.close_lane(&mut state, &from)?;
        }
        self.sync_states(&state);
        self.emit(&BranchEvent::switched(from.clone(), to.clone()));
        Ok(Switched::new(from, to.clone()))
    }

    fn rename(
        &self,
        branch: &BranchId,
        name: BranchName,
        title: Option<String>,
        by: &Actor,
    ) -> Result<BranchInfo, BranchError> {
        let state = lock(&self.state);
        let info = self.live(branch)?;
        Self::may_manage(&state, by, &info, "rename")?;
        let renamed = self.shared.refs.rename(branch, name.clone(), title, self.shared.snapshots.store().as_ref())?;
        self.emit(&BranchEvent::renamed(branch.clone(), name));
        Ok(self.filled(renamed))
    }

    fn delete(&self, branch: &BranchId, by: &Actor) -> Result<(), BranchError> {
        let state = lock(&self.state);
        let info = self.live(branch)?;
        Self::may_manage(&state, by, &info, "delete")?;
        if branch.is_main() || state.folder == *branch || state.lanes.contains_key(branch) {
            return Err(BranchError::Refused(
                "`main`, the folder branch and a branch with an open lane can't be deleted".to_owned(),
            ));
        }
        self.shared.refs.delete(branch, self.shared.wall.now(), self.shared.snapshots.store().as_ref())?;
        self.emit(&BranchEvent::deleted(branch.clone()));
        Ok(())
    }

    fn undelete(&self, branch: &BranchId, by: &Actor) -> Result<BranchInfo, BranchError> {
        let state = lock(&self.state);
        let info = match self.shared.refs.read(branch)? {
            Some(info) if info.deleted.is_some() => info,
            _ => return Err(BranchError::NotFound(branch.to_string())),
        };
        Self::may_manage(&state, by, &info, "bring back")?;
        let back = self.shared.refs.undelete(branch, self.shared.snapshots.store().as_ref())?;
        self.emit(&BranchEvent::undeleted(branch.clone()));
        Ok(self.filled(back))
    }

    fn review(&self, branch: &BranchId, decision: ReviewDecision, by: &Actor) -> Result<BranchInfo, BranchError> {
        person_only(by, "deciding a review")?;
        let _state = lock(&self.state);
        let decided = match decision {
            ReviewDecision::MarkMerged => ReviewState::Merged,
            ReviewDecision::SendBack { .. } => ReviewState::SentBack,
            _ => return Err(BranchError::Invalid("this kind of review decision is not known here".to_owned())),
        };
        let info = self.shared.refs.update(branch, self.shared.snapshots.store().as_ref(), |info| {
            let name = info.name.clone();
            match info.review.as_mut().filter(|r| r.state == ReviewState::Ready) {
                Some(review) => {
                    review.state = decided;
                    if let ReviewDecision::SendBack { comment, .. } = &decision {
                        review.note = Some(comment.clone());
                    }
                    Ok(())
                }
                None => Err(BranchError::Refused(format!("branch {name} has no review waiting"))),
            }
        })?;
        self.emit(&BranchEvent::reviewed(branch.clone(), decided));
        Ok(self.filled(info))
    }

    fn restore(&self, branch: &BranchId, to: StateRef, by: &Actor, grants: &Grants) -> Result<Restored, BranchError> {
        let mut state = lock(&self.state);
        self.ensure_main(&mut state)?;
        let info = self.live(branch)?;
        let to = self.normalized(&info, &to);
        if to == StateRef::Now {
            return Err(BranchError::Invalid("restoring to `now` changes nothing".to_owned()));
        }
        let lane = self.lane_locked(&mut state, branch)?;
        // Everything that can refuse comes before the first change: an open transaction blocks recording the state
        // now, and the restore itself is planned (as `by`, with `grants`) before anything is written.
        let committed = lane.executor.committed().map_err(|e| BranchError::Busy(e.message))?;
        if let Some((_, _, label)) = committed.open.first() {
            return Err(BranchError::Busy(format!("'{label}' is still open: finish or cancel it first")));
        }
        let request = |mode| Request {
            op: "history.restore".to_owned(),
            args: json!({ "to": to }),
            mode,
            actor: by.clone(),
            grants: grants.clone(),
            cause: None,
            tx: None,
            base: None,
        };
        lane.executor.run(request(Mode::DryRun)).map_err(op_refusal)?;
        let made = Made { kind: PointKind::Restore, label: "Before restore", by, advance: true };
        let (point, point_id) = self.record(&lane, &info, &made, None)?;
        let (slug, clock) = stamp(self.shared.wall.now());
        let name = self.free_name(&format!("restore/{slug}"))?;
        let mut backup = self.new_ref(name, branch, (&point, point_id), by)?;
        backup.title = Some(format!("Before restore {clock}"));
        backup.auto = true;
        self.shared.refs.create(&backup, self.shared.snapshots.store().as_ref())?;
        if let Err(error) = lane.executor.run(request(Mode::Apply)) {
            if let Err(removing) = self.shared.refs.remove(&backup.id) {
                tracing::warn!(%removing, "the automatic branch of a failed restore couldn't be removed");
            }
            return Err(op_refusal(error));
        }
        let backup = self.filled(backup);
        self.emit(&BranchEvent::created(backup.clone()));
        Ok(Restored::new(backup, to))
    }

    fn lane(&self, branch: &BranchId) -> Result<Lane, BranchError> {
        let mut state = lock(&self.state);
        // `main` has a lane before it has a ref: the ref is made the first time anything branches.
        if !branch.is_main() {
            self.live(branch)?;
        }
        let lane = self.lane_locked(&mut state, branch)?;
        self.sync_states(&state);
        Ok(Lane::new(lane.executor.clone() as Arc<dyn Runner>, lane.executor.clone() as Arc<dyn Sessions>))
    }

    fn subscribe(&self) -> Receiver<BranchEvent> {
        let (tx, rx) = channel();
        lock(&self.subscribers).push(tx);
        rx
    }
}

/// An op's refusal as a branch error: a missing scope is `Denied`, anything else says why.
fn op_refusal(error: OpError) -> BranchError {
    match error.code {
        omuzaki_ops_api::ErrorCode::Denied => BranchError::Denied(error.message),
        _ => BranchError::Invalid(match error.hint {
            Some(hint) => format!("{} ({hint})", error.message),
            None => error.message,
        }),
    }
}

/// The executor's job manager, for a host that needs it.
pub fn jobs_of(lane: &OpenLane) -> Arc<dyn Jobs> {
    lane.executor.clone()
}

/// The executor of a lane, for a host that needs what only it offers.
pub fn executor_of(lane: &OpenLane) -> &Arc<Executor> {
    &lane.executor
}
