//! [`Automation`]: the app's one automation engine, shared as a service. It is the [`EventSink`] every producer
//! (the platform adapters, the OBS client, hotkeys) publishes into, and the [`AutomationControl`] the runtime ops call.
//!
//! One show runs at a time (design D5). A run is two threads: the **engine thread** owns the show's
//! [`Core`] and does all node work, sleeping until its next wake or inbox item; the **dispatcher** takes the op calls
//! each step asks for and runs them, in order, through the attached [`Runner`], posting each reply back to the inbox.
//! The engine thread never takes the executor's lock, so an op that starts, stops or feeds the engine from inside the
//! executor never waits on it.
//!
//! **Who acts.** Calls run in the show's name (`Actor::System`, with the cause naming the show, graph, node and
//! event) with the base automation grants `read`, `perform`, `broadcast` and `render`; a call in a token rooted at
//! `automation.fire` runs as the caller instead (design D7). The approval flow that adds grants per show (D11) and
//! the per-run automation session (ADR 0020) come with the leaves.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::{Clock, Seconds, SystemClock};
use omuzaki_live_api::event::{EventId, EventSink, LiveEvent, PublishError, Source, check_payload, find_event_kind};
use omuzaki_live_api::ids::{EventKind, GraphId, GroupId, ShowId, VarName};
use omuzaki_live_api::limits::MONITOR_WATCHED_SECONDS;
use omuzaki_live_api::services::{
    Approvals, AutomationControl, Dropped, PersistScope, RunInfo, ShowApprovals, ShowKey, Status, SystemWallClock,
    ValuesSnapshot, VariableStore, WallClock,
};
use omuzaki_live_api::show::{Finding, ShowDef, VarScope, paths};
use omuzaki_live_api::value::Val;
use omuzaki_ops_api::{Actor, Event, Grants, OpError, Runner, Scope, SessionId, Sessions};
use serde_json::Value;

use crate::compile::{Arming, FileHasher, ProgramHasher, compile_with};
use crate::core::{Control, Core, Item};
use crate::dispatch::{Dispatch, Work};
use crate::inbox::Inbox;
use crate::vars;

/// How long the show's document must stay still before an edit reloads the running show (design D5).
const RELOAD_SETTLE: Duration = Duration::from_millis(100);
/// The longest the engine thread sleeps without looking at its stop flag.
const IDLE_WAIT: Duration = Duration::from_millis(250);
/// The values monitor refreshes at most this often (design D10: four times a second).
const VALUES_INTERVAL: Duration = Duration::from_millis(250);
/// The most inbox items the engine thread takes per lock. Small, so the droppable queue's bound still means what it
/// says, and large enough that the engine rarely contends with producers.
const BATCH: usize = 64;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the engine thread publishes for the status and values queries, which never wait on it.
#[derive(Default)]
struct Shared {
    status: Mutex<Status>,
    values: Mutex<Option<Arc<ValuesSnapshot>>>,
    producer_dropped: AtomicU64,
    /// When `automation.values` was last read, on the runner clock: the snapshot is refreshed only while someone
    /// read it in the last [`MONITOR_WATCHED_SECONDS`] (design D10), so an unwatched show pays nothing.
    read_at: Mutex<Option<Seconds>>,
    /// How many times the engine refreshed the snapshot.
    refreshes: AtomicU64,
}

/// Where variable values persist: the store, and the project folder a show's own values live under (none: `show`
/// values last one run, like `session` ones).
struct Persistence {
    store: Arc<dyn VariableStore>,
    project: Option<String>,
}

/// One values file to write, handed to the writer thread.
type Save = (PersistScope, std::collections::BTreeMap<VarName, Value>);

/// The writer thread of one run: saves in order, off the engine thread (design D10).
struct Writer {
    saves: Sender<Save>,
    thread: JoinHandle<()>,
}

impl Writer {
    fn start(store: Arc<dyn VariableStore>, show: &ShowId) -> Option<Writer> {
        let (saves, pending) = channel::<Save>();
        let thread = std::thread::Builder::new()
            .name(format!("automation-values-{show}"))
            .spawn(move || {
                for (scope, values) in pending {
                    // A failed write keeps the old file (the store writes atomically); the next change tries again.
                    let _ = store.save(&scope, &values);
                }
            })
            .ok()?;
        Some(Writer { saves, thread })
    }
}

/// Where a run's approvals come from (design D11): read again on every reload, so an approval arms its nodes in the
/// running show.
#[derive(Clone)]
struct ArmingSource {
    approvals: Option<Arc<dyn Approvals>>,
    key: ShowKey,
    hasher: Arc<dyn ProgramHasher>,
}

impl ArmingSource {
    fn approvals(&self) -> ShowApprovals {
        self.approvals.as_ref().map(|a| a.show(&self.key)).unwrap_or_default()
    }
}

/// A running show.
struct Running {
    info: RunInfo,
    /// What it was compiled from (the last reload's), to check a change from outside before it is queued.
    def: Arc<Mutex<ShowDef>>,
    inbox: Arc<Inbox>,
    stop: Arc<AtomicBool>,
    engine: Option<JoinHandle<()>>,
    dispatch: Arc<Dispatch>,
    /// The ordered worker's queue, for pausing and resuming the session in order with the calls.
    work: Sender<Work>,
}

/// The automation engine. Build it once, register it as a service (and as `dyn EventSink` and
/// `dyn AutomationControl`), then [`attach`](Self::attach) the app's runner once the executor exists.
pub struct Automation {
    runner: Mutex<Option<Weak<dyn Runner>>>,
    sessions: Mutex<Option<Weak<dyn Sessions>>>,
    clock: Arc<dyn Clock>,
    wall: Arc<dyn WallClock>,
    run: Mutex<Option<Running>>,
    shared: Arc<Shared>,
    next_event: Arc<AtomicU64>,
    persistence: Mutex<Option<Persistence>>,
    /// What the last start found: values files it couldn't read, stored values of another type, and the compile's
    /// findings (inert nodes and why).
    start_findings: Mutex<Vec<Finding>>,
    /// What this machine approved per show (design D11); without it, nothing is.
    approvals: Mutex<Option<Arc<dyn Approvals>>>,
    /// How program files are hashed for the program rule.
    hasher: Mutex<Arc<dyn ProgramHasher>>,
}

impl std::fmt::Debug for Automation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Automation")
            .field("running", &lock(&self.run).as_ref().map(|r| &r.info))
            .finish_non_exhaustive()
    }
}

impl Default for Automation {
    fn default() -> Self {
        Self::new()
    }
}

impl Automation {
    /// An engine on the system clocks, with no runner attached and no show running.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clocks(Arc::new(SystemClock::new()), Arc::new(SystemWallClock))
    }

    /// An engine on these clocks: the runner clock nodes read as `cx.now()` and the wall clock they read as `cx.utc()`.
    #[must_use]
    pub fn with_clocks(clock: Arc<dyn Clock>, wall: Arc<dyn WallClock>) -> Self {
        Self {
            runner: Mutex::new(None),
            sessions: Mutex::new(None),
            clock,
            wall,
            run: Mutex::new(None),
            shared: Arc::new(Shared::default()),
            next_event: Arc::new(AtomicU64::new(0)),
            persistence: Mutex::new(None),
            start_findings: Mutex::new(Vec::new()),
            approvals: Mutex::new(None),
            hasher: Mutex::new(Arc::new(FileHasher)),
        }
    }

    /// Where approvals are read from (design D11): at every start and reload. Without a store nothing risky arms.
    pub fn set_approvals(&self, approvals: Arc<dyn Approvals>) {
        *lock(&self.approvals) = Some(approvals);
    }

    /// How program files are hashed for the program rule (the real files by default).
    pub fn set_hasher(&self, hasher: Arc<dyn ProgramHasher>) {
        *lock(&self.hasher) = hasher;
    }

    /// The approvals key and source for show `id`.
    fn arming_source(&self, id: &ShowId) -> ArmingSource {
        let project = lock(&self.persistence).as_ref().and_then(|p| p.project.clone()).unwrap_or_default();
        ArmingSource {
            approvals: lock(&self.approvals).clone(),
            key: ShowKey::new(project, id.clone()),
            hasher: Arc::clone(&lock(&self.hasher)),
        }
    }

    /// Swaps `def` into the running show between two steps (design D5's hot reload); nothing when no show runs.
    /// Edits made through the runner reload on their own; this is for a show that changes some other way.
    pub fn reload(&self, def: ShowDef) {
        if let Some(running) = lock(&self.run).as_ref() {
            running.inbox.push_reply(Item::Reload(Box::new(def)));
        }
    }

    /// Recompiles the running show against the approvals as they are now: what the UI calls after a person allows
    /// or revokes something, so the nodes it arms start without a restart.
    pub fn rearm(&self) {
        let def = lock(&self.run).as_ref().map(|r| lock(&r.def).clone());
        if let Some(def) = def {
            self.reload(def);
        }
    }

    /// Where `show` and `global` variable values persist from the next start on (design D10): `store`, with a show's
    /// own values under `project` (the project folder's real path; `None` keeps them for one run only).
    pub fn set_variables(&self, store: Arc<dyn VariableStore>, project: Option<String>) {
        *lock(&self.persistence) = Some(Persistence { store, project });
    }

    /// How many times the engine refreshed the values snapshot this process: it stops while nobody reads
    /// `automation.values` (design D10).
    pub fn values_refreshes(&self) -> u64 {
        self.shared.refreshes.load(Ordering::Relaxed)
    }

    /// What the last start found about the stored values: files it couldn't read and values of another type. None of
    /// them stops a start; the variables they name start from their defaults.
    pub fn start_findings(&self) -> Vec<Finding> {
        lock(&self.start_findings).clone()
    }

    /// Loads the stored values into `core`, returning the writer and the file each persisted scope writes to.
    fn load_values(&self, id: &ShowId, core: &mut Core) -> (Option<Writer>, Vec<(VarScope, PersistScope)>) {
        let persistence = lock(&self.persistence);
        let Some(persistence) = persistence.as_ref() else {
            lock(&self.start_findings).clear();
            return (None, Vec::new());
        };
        let (findings, files) = vars::load(core, persistence.store.as_ref(), persistence.project.as_deref(), id);
        *lock(&self.start_findings) = findings;
        (Writer::start(Arc::clone(&persistence.store), id), files)
    }

    /// Attaches the runner the show's op calls go through. Held weakly, so the engine (a service of the executor)
    /// doesn't keep the executor alive: the app keeps its own `Arc` for as long as it runs.
    pub fn attach(&self, runner: &Arc<dyn Runner>) {
        *lock(&self.runner) = Some(Arc::downgrade(runner));
    }

    /// Attaches the executor's sessions: a show started by a person (or by an agent working for one) then runs in an
    /// automation session of its own (design D7). Held weakly, like the runner.
    pub fn attach_sessions(&self, sessions: &Arc<dyn Sessions>) {
        *lock(&self.sessions) = Some(Arc::downgrade(sessions));
    }

    /// The person a start by `by` runs on behalf of: `by` itself, or the person an agent's session works for.
    fn principal(&self, by: &Actor) -> Option<Actor> {
        if let Actor::Person { .. } = by {
            return Some(by.clone());
        }
        let sessions = lock(&self.sessions).as_ref().and_then(Weak::upgrade)?;
        sessions.session_of(by).map(|s| s.on_behalf_of)
    }

    fn runner(&self) -> Option<Weak<dyn Runner>> {
        lock(&self.runner).clone()
    }

    /// Compiles and starts `def` as show `id` in Omuzaki's own name (no session): what tests and tools use.
    ///
    /// # Errors
    /// [`OpError::conflict`] when a show already runs.
    pub fn start_def(&self, id: &ShowId, def: &ShowDef) -> Result<RunInfo, OpError> {
        self.start_for(id, def, None)
    }

    /// Compiles and starts `def` as show `id`, started by `by`: the show runs in an automation session on behalf of
    /// `by`'s person when the executor's sessions are attached. What `automation.start` uses, since it reads the show
    /// from the document it already holds.
    ///
    /// # Errors
    /// [`OpError::conflict`] when a show already runs.
    pub fn start_def_by(&self, id: &ShowId, def: &ShowDef, by: &Actor) -> Result<RunInfo, OpError> {
        self.start_for(id, def, self.principal(by))
    }

    fn start_for(&self, id: &ShowId, def: &ShowDef, principal: Option<Actor>) -> Result<RunInfo, OpError> {
        let mut run = lock(&self.run);
        if let Some(running) = run.as_ref()
            && running.engine.as_ref().is_some_and(|h| !h.is_finished())
        {
            return Err(OpError::conflict(format!("show `{}` is running", running.info.show))
                .hint(format!("stop `{}` first with automation.stop", running.info.show)));
        }
        let seed = def.seed.unwrap_or_else(draw_seed);
        let at = self.clock.now();
        let info = RunInfo::new(id.clone(), seed, SessionId(0), at);
        let arming = self.arming_source(id);
        let approvals = arming.approvals();
        let compiled = compile_with(id, def, Arming { approvals: &approvals, hasher: arming.hasher.as_ref() }, None);
        let compiled_findings = compiled.findings.clone();
        let mut core = Core::new(compiled, seed, at, self.wall.now());
        let (writer, files) = self.load_values(id, &mut core);
        lock(&self.start_findings).extend(compiled_findings);
        let shared_def = Arc::new(Mutex::new(def.clone()));
        let inbox = Arc::new(Inbox::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (work, pending) = channel();
        let sessions = lock(&self.sessions).clone();
        let dispatch =
            Dispatch::new(self.runner(), sessions, id, principal, Arc::clone(&inbox), Arc::clone(&self.clock));
        dispatch.spawn(pending)?;
        *lock(&self.shared.status) = Status::default().with_running(info.clone()).with_graphs(core.graphs());
        // A first snapshot before the engine thread runs, so a read right after the start never finds the last run's.
        *lock(&self.shared.values) = Some(Arc::new(core.values()));
        let engine = EngineThread {
            core,
            inbox: Arc::clone(&inbox),
            stop: Arc::clone(&stop),
            clock: Arc::clone(&self.clock),
            wall: Arc::clone(&self.wall),
            calls: work.clone(),
            dispatch: Arc::clone(&dispatch),
            shared: Arc::clone(&self.shared),
            next_event: Arc::clone(&self.next_event),
            info: info.clone(),
            writer,
            files,
            arming,
            def: Arc::clone(&shared_def),
        };
        let handle = std::thread::Builder::new()
            .name(format!("automation-{id}"))
            .spawn(move || engine.run())
            .map_err(|e| OpError::internal(format!("the automation engine didn't start: {e}")))?;
        if let Some(runner) = self.runner() {
            watch_edits(&runner, id, def, &inbox, &stop);
        }
        *run = Some(Running { info: info.clone(), def: shared_def, inbox, stop, engine: Some(handle), dispatch, work });
        Ok(info)
    }

    /// The running show, if any.
    #[must_use]
    pub fn running(&self) -> Option<RunInfo> {
        lock(&self.run).as_ref().filter(|r| r.engine.as_ref().is_some_and(|h| !h.is_finished())).map(|r| r.info.clone())
    }

    /// Queues `work` for the running show's ordered worker.
    fn session_work(&self, work: Work) {
        if let Some(running) = lock(&self.run).as_ref() {
            // The worker only goes away with the run.
            let _ = running.work.send(work);
        }
    }

    /// Checks `change` against the running show and queues it as a step of its own.
    fn control(&self, change: impl FnOnce(&ShowDef) -> Result<Control, OpError>) -> Result<(), OpError> {
        let run = lock(&self.run);
        let running =
            run.as_ref().filter(|r| r.engine.as_ref().is_some_and(|h| !h.is_finished())).ok_or_else(not_running)?;
        let control = change(&lock(&running.def))?;
        running.inbox.push_reply(Item::Control(control));
        Ok(())
    }

    /// Queues an event for the running show after `publish`'s checks; `Ok` with nothing done when no show runs.
    fn enqueue(&self, event: LiveEvent, droppable: bool) -> Result<Option<EventId>, PublishError> {
        let run = lock(&self.run);
        let Some(running) = run.as_ref() else { return Ok(None) };
        // The inbox stamps arrival on the engine clock (producers run on their own threads with no runner clock) and
        // numbers the event under its own lock, so ids rise in the order events are queued.
        running.inbox.push_event(event, droppable, self.clock.now(), &self.next_event)
    }
}

/// Follows the runner's events for edits under show `id` and queues a reload once they settle for
/// [`RELOAD_SETTLE`] (design D5). An undo or redo carries no patch, so it always rereads. The thread ends with the
/// run, or when the runner goes away.
fn watch_edits(runner: &Weak<dyn Runner>, id: &ShowId, def: &ShowDef, inbox: &Arc<Inbox>, stop: &Arc<AtomicBool>) {
    let grants = Grants::of([Scope::Read]);
    let Some(events) = runner.upgrade().and_then(|r| r.subscribe(&grants).ok()) else { return };
    let (runner, inbox, stop) = (Weak::clone(runner), Arc::clone(inbox), Arc::clone(stop));
    let (path, mut last) = (paths::show(id), def.clone());
    let spawned = std::thread::Builder::new().name(format!("automation-edits-{id}")).spawn(move || {
        let mut due: Option<Instant> = None;
        while !stop.load(Ordering::SeqCst) {
            let wait = due.map_or(IDLE_WAIT, |d| d.saturating_duration_since(Instant::now()).min(IDLE_WAIT));
            match events.recv_timeout(wait) {
                Ok(event) => {
                    let touches = match &event {
                        Event::OpApplied { record, .. } => {
                            record.patch.changes.iter().any(|c| c.path.starts_with(&path) || path.starts_with(&c.path))
                        }
                        Event::Undone { .. } | Event::Redone { .. } | Event::TxRolledBack { .. } => true,
                        _ => false,
                    };
                    if touches {
                        due = Some(Instant::now() + RELOAD_SETTLE);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if due.is_some_and(|d| Instant::now() >= d) {
                due = None;
                let Some(runner) = runner.upgrade() else { break };
                // A show deleted while it runs keeps running as it was; stopping it is its own op.
                let read = runner.get(&path, &grants).ok().flatten().and_then(|node| ShowDef::from_node(&node).ok());
                if let Some(def) = read.filter(|def| *def != last) {
                    last = def.clone();
                    inbox.push_reply(Item::Reload(Box::new(def)));
                }
            }
        }
    });
    // Without the thread the show still runs; it just doesn't follow edits until it restarts.
    drop(spawned);
}

/// A seed for a show that pins none, from the OS clock and the process (reported in the status so a run can be
/// reproduced by pinning it).
fn draw_seed() -> u32 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mixed = nanos ^ (u128::from(std::process::id()) << 32);
    // Folded to 32 bits on purpose: the seed is a u32 (design D6).
    #[allow(clippy::cast_possible_truncation)]
    let folded = (mixed ^ (mixed >> 64) ^ (mixed >> 32)) as u32;
    folded
}

impl EventSink for Automation {
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError> {
        if matches!(event.source, Source::Manual { .. }) {
            return Err(PublishError::ManualSource);
        }
        if event.acting.is_some() {
            return Err(PublishError::Acting);
        }
        check_payload(&event.kind, &event.payload)?;
        let droppable = find_event_kind(event.kind.as_str()).is_some_and(|k| k.droppable);
        self.enqueue(event, droppable).map(|_| ())
    }

    fn producer_dropped(&self, count: u64) {
        self.shared.producer_dropped.fetch_add(count, Ordering::Relaxed);
    }
}

fn not_running() -> OpError {
    OpError::conflict("no show is running").hint("start one with automation.start")
}

impl AutomationControl for Automation {
    fn start(&self, show: &ShowId, by: &Actor) -> Result<RunInfo, OpError> {
        let runner = self.runner().and_then(|w| w.upgrade()).ok_or_else(|| OpError::unavailable("runner"))?;
        let node = runner
            .get(&paths::show(show), &Grants::of([Scope::Read]))?
            .ok_or_else(|| OpError::not_found(format!("there is no show `{show}`")))?;
        let def = ShowDef::from_node(&node).map_err(|e| OpError::invalid("show", e.to_string()))?;
        self.start_def_by(show, &def, by)
    }

    fn stop(&self, _by: &Actor) -> Result<(), OpError> {
        let mut running = lock(&self.run).take().ok_or_else(not_running)?;
        running.stop.store(true, Ordering::SeqCst);
        running.inbox.close();
        // The engine thread never waits on the executor, so joining it here (even from inside an op) is safe.
        if let Some(handle) = running.engine.take() {
            let _ = handle.join();
        }
        // Calls not yet started are dropped; the session ends once the ones in flight are back (at most 2 s).
        running.dispatch.finish();
        Ok(())
    }

    fn pause(&self, _by: &Actor) -> Result<(), OpError> {
        self.control(|_| Ok(Control::Pause))?;
        self.session_work(Work::Pause(true));
        Ok(())
    }

    fn resume(&self, _by: &Actor) -> Result<(), OpError> {
        self.control(|_| Ok(Control::Resume))?;
        self.session_work(Work::Pause(false));
        Ok(())
    }

    fn status(&self) -> Status {
        if self.running().is_none() {
            return Status::default();
        }
        lock(&self.shared.status).clone()
    }

    fn values(&self) -> Option<Arc<ValuesSnapshot>> {
        self.running()?;
        *lock(&self.shared.read_at) = Some(self.clock.now());
        lock(&self.shared.values).clone()
    }

    fn set_variable(&self, name: &VarName, value: Value, _by: &Actor) -> Result<(), OpError> {
        self.control(|def| {
            let var = def
                .variables
                .get(name)
                .ok_or_else(|| OpError::not_found(format!("the show declares no variable `{name}`")))?;
            let ty = var.ty.port_type();
            let value = Val::from_json(&value, ty)
                .ok()
                .filter(|v| v.ty() == ty && v.check().is_ok())
                .ok_or_else(|| OpError::invalid("value", format!("`{name}` holds {ty:?}")))?;
            Ok(Control::SetVariable { name: name.clone(), value })
        })
    }

    fn switch_group(&self, group: &GroupId, on: bool, _by: &Actor) -> Result<(), OpError> {
        self.control(|def| {
            if !def.groups.contains_key(group) {
                return Err(OpError::not_found(format!("the show declares no group `{group}`")));
            }
            Ok(Control::SwitchGroup { group: group.clone(), on })
        })
    }

    fn resume_graph(&self, graph: &GraphId, _by: &Actor) -> Result<(), OpError> {
        self.control(|def| {
            if !def.graphs.contains_key(graph) {
                return Err(OpError::not_found(format!("the show has no graph `{graph}`")));
            }
            Ok(Control::ResumeGraph { graph: graph.clone() })
        })
    }

    fn fire(&self, kind: EventKind, payload: Value, by: &Actor) -> Result<EventId, OpError> {
        check_payload(&kind, &payload).map_err(|e| OpError::invalid("payload", e.to_string()))?;
        let droppable = find_event_kind(kind.as_str()).is_some_and(|k| k.droppable);
        let event = LiveEvent::new(kind, Source::manual(by.clone()), self.clock.now(), payload).with_acting(by.clone());
        match self.enqueue(event, droppable) {
            Ok(Some(id)) => Ok(id),
            Ok(None) => Err(not_running()),
            Err(e) => Err(OpError::conflict(e.to_string())),
        }
    }
}

impl Drop for Automation {
    fn drop(&mut self) {
        if let Some(running) = lock(&self.run).take() {
            running.stop.store(true, Ordering::SeqCst);
            running.inbox.close();
        }
    }
}

/// The engine thread of one run.
struct EngineThread {
    core: Core,
    inbox: Arc<Inbox>,
    stop: Arc<AtomicBool>,
    clock: Arc<dyn Clock>,
    wall: Arc<dyn WallClock>,
    calls: Sender<Work>,
    dispatch: Arc<Dispatch>,
    shared: Arc<Shared>,
    next_event: Arc<AtomicU64>,
    info: RunInfo,
    writer: Option<Writer>,
    /// Which file each persisted scope writes to.
    files: Vec<(VarScope, PersistScope)>,
    /// Where the approvals a reload compiles against come from.
    arming: ArmingSource,
    /// The show as last compiled, shared with the service's checks.
    def: Arc<Mutex<ShowDef>>,
}

impl EngineThread {
    /// Hands the values files due now (with `flush`, every one the run changed) to the writer thread.
    fn save(&mut self, flush: bool) {
        let Some(writer) = &self.writer else { return };
        for (scope, values) in self.core.take_saves(self.clock.now(), flush) {
            if let Some((_, persist)) = self.files.iter().find(|(s, _)| *s == scope) {
                // The writer only ends after this thread drops its sender.
                let _ = writer.saves.send((persist.clone(), values));
            }
        }
    }

    /// Recompiles against `def` and the approvals as they are now, and swaps it in (design D5).
    fn reload(&mut self, def: ShowDef) {
        let approvals = self.arming.approvals();
        let arming = Arming { approvals: &approvals, hasher: self.arming.hasher.as_ref() };
        let compiled = compile_with(&self.info.show, &def, arming, Some(self.core.compiled_nodes()));
        self.core.reload(compiled, self.clock.now(), self.wall.now());
        *lock(&self.def) = def;
    }

    fn run(mut self) {
        self.core.start(self.clock.now(), self.wall.now());
        let mut values_at: Option<Instant> = None;
        // Whether a step ran since the status and the values were last published: an idle engine republishes nothing,
        // and a busy one at most every `VALUES_INTERVAL`, so producers never wait behind the monitor.
        let mut dirty = true;
        let mut batch = std::collections::VecDeque::with_capacity(BATCH);
        loop {
            for call in self.core.take_outgoing() {
                // The dispatcher only goes away with this thread; a send can't fail before then.
                let _ = self.calls.send(Work::Call(call));
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let idle = batch.is_empty() && self.inbox.is_empty();
            if dirty && (idle || values_at.is_none_or(|t| t.elapsed() >= VALUES_INTERVAL)) {
                self.publish_status();
                // The first snapshot always; later ones while someone watches.
                if values_at.is_none() || self.watched() {
                    *lock(&self.shared.values) = Some(Arc::new(self.core.values()));
                    self.shared.refreshes.fetch_add(1, Ordering::Relaxed);
                    values_at = Some(Instant::now());
                    dirty = false;
                }
            }
            if let Some(mut event) = self.core.pop_own() {
                event.id = EventId(self.next_event.fetch_add(1, Ordering::Relaxed) + 1);
                self.core.step(self.clock.now(), self.wall.now(), Item::Event(event));
                dirty = true;
                continue;
            }
            let now = self.clock.now();
            if self.core.wake(now, self.wall.now()) {
                dirty = true;
                continue;
            }
            self.save(false);
            let next = match (self.core.next_wake(), self.core.next_save()) {
                (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
                (a, b) => a.or(b),
            };
            let wait = next.map_or(IDLE_WAIT, |due| {
                Duration::try_from_secs_f64((due.0 - now.0).max(0.0)).unwrap_or(IDLE_WAIT).min(IDLE_WAIT)
            });
            if batch.is_empty() {
                self.inbox.pop_batch(wait, BATCH, &mut batch);
            }
            if let Some(item) = batch.pop_front() {
                match item {
                    Item::Reload(def) => self.reload(*def),
                    item => self.core.step(self.clock.now(), self.wall.now(), item),
                }
                dirty = true;
            } else {
                // The producers' and the inbox's drop counts change without a step.
                self.publish_status();
            }
        }
        self.core.stop(self.clock.now(), self.wall.now());
        self.publish_status();
        *lock(&self.shared.values) = Some(Arc::new(self.core.values()));
        // The final flush, written before the stop returns.
        self.save(true);
        if let Some(Writer { saves, thread }) = self.writer.take() {
            drop(saves);
            let _ = thread.join();
        }
    }

    /// Whether someone read the values in the last [`MONITOR_WATCHED_SECONDS`].
    fn watched(&self) -> bool {
        let now = self.clock.now();
        lock(&self.shared.read_at).is_some_and(|at| now.0 - at.0 <= MONITOR_WATCHED_SECONDS)
    }

    fn publish_status(&self) {
        let (inbox, inbox_full, duplicate) = self.inbox.dropped();
        let mut dropped: Dropped = self.core.dropped();
        dropped.inbox = inbox;
        dropped.inbox_full = inbox_full;
        dropped.duplicate = duplicate;
        dropped.producer = self.shared.producer_dropped.load(Ordering::Relaxed);
        let mut info = self.info.clone();
        info.session = self.dispatch.session_id();
        *lock(&self.shared.status) = Status::default()
            .with_running(info)
            .with_paused(self.core.paused())
            .with_graphs(self.core.graphs())
            .with_dropped(dropped);
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::time::Seconds;

    use super::*;

    #[test]
    fn seeds_differ_between_draws() {
        let a = draw_seed();
        std::thread::sleep(Duration::from_millis(2));
        assert_ne!(a, draw_seed());
    }

    #[test]
    fn a_show_runs_with_the_base_grants_only() {
        let grants = crate::dispatch::show_grants();
        assert!(grants.allows(Scope::Perform) && !grants.allows(Scope::Fs) && !grants.allows(Scope::EditScene));
    }

    #[test]
    fn nothing_running_answers_an_empty_status() {
        let automation = Automation::new();
        assert_eq!(automation.status(), Status::default());
        assert!(automation.values().is_none());
        let at = Seconds(0.0);
        let event = LiveEvent::new(EventKind::from_static("custom.ping"), Source::Test, at, serde_json::json!({}));
        assert_eq!(automation.publish(event), Ok(()), "nobody listens; the event is gone");
    }
}
