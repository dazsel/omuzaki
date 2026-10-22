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
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::{Clock, Seconds, SystemClock};
use omuzaki_live_api::event::{EventId, EventSink, LiveEvent, PublishError, Source, check_payload, find_event_kind};
use omuzaki_live_api::ids::{EventKind, GraphId, GroupId, ShowId, VarName};
use omuzaki_live_api::limits::MONITOR_WATCHED_SECONDS;
use omuzaki_live_api::services::{
    AutomationControl, Dropped, PersistScope, RunInfo, Status, SystemWallClock, ValuesSnapshot, VariableStore,
    WallClock,
};
use omuzaki_live_api::show::{Finding, ShowDef, VarScope, paths};
use omuzaki_live_api::value::Val;
use omuzaki_ops_api::{Actor, Cause, CauseKind, Grants, Mode, OpError, Request, Runner, Scope, SessionId};
use serde_json::Value;

use crate::compile::compile;
use crate::core::{Control, Core, Item, OpRequest};
use crate::inbox::Inbox;
use crate::vars;

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

/// The grants a show's calls run with until approvals add to them (design D7, D11).
fn show_grants() -> Grants {
    Grants::of([Scope::Read, Scope::Perform, Scope::Broadcast, Scope::Render])
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

/// A running show.
struct Running {
    info: RunInfo,
    /// What it was compiled from, to check a change from outside before it is queued.
    def: ShowDef,
    inbox: Arc<Inbox>,
    stop: Arc<AtomicBool>,
    engine: Option<JoinHandle<()>>,
}

/// The automation engine. Build it once, register it as a service (and as `dyn EventSink` and
/// `dyn AutomationControl`), then [`attach`](Self::attach) the app's runner once the executor exists.
pub struct Automation {
    runner: Mutex<Option<Weak<dyn Runner>>>,
    clock: Arc<dyn Clock>,
    wall: Arc<dyn WallClock>,
    run: Mutex<Option<Running>>,
    shared: Arc<Shared>,
    next_event: Arc<AtomicU64>,
    persistence: Mutex<Option<Persistence>>,
    /// What the last start found: values files it couldn't read, stored values of another type.
    start_findings: Mutex<Vec<Finding>>,
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
            clock,
            wall,
            run: Mutex::new(None),
            shared: Arc::new(Shared::default()),
            next_event: Arc::new(AtomicU64::new(0)),
            persistence: Mutex::new(None),
            start_findings: Mutex::new(Vec::new()),
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

    fn runner(&self) -> Option<Weak<dyn Runner>> {
        lock(&self.runner).clone()
    }

    /// Compiles and starts `def` as show `id`. What a start from inside the executor uses (`automation.start`), since
    /// it reads the show from the document it already holds.
    ///
    /// # Errors
    /// [`OpError::conflict`] when a show already runs.
    pub fn start_def(&self, id: &ShowId, def: &ShowDef) -> Result<RunInfo, OpError> {
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
        let compiled = compile(id, def);
        let mut core = Core::new(compiled, seed, at, self.wall.now());
        let (writer, files) = self.load_values(id, &mut core);
        let inbox = Arc::new(Inbox::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (calls, pending) = channel();
        let dispatcher = Dispatcher { runner: self.runner(), inbox: Arc::clone(&inbox) };
        std::thread::Builder::new()
            .name(format!("automation-calls-{id}"))
            .spawn(move || dispatcher.run(&pending))
            .map_err(|e| OpError::internal(format!("the automation dispatcher didn't start: {e}")))?;
        *lock(&self.shared.status) = Status::default().with_running(info.clone()).with_graphs(core.graphs());
        // A first snapshot before the engine thread runs, so a read right after the start never finds the last run's.
        *lock(&self.shared.values) = Some(Arc::new(core.values()));
        let engine = EngineThread {
            core,
            inbox: Arc::clone(&inbox),
            stop: Arc::clone(&stop),
            clock: Arc::clone(&self.clock),
            wall: Arc::clone(&self.wall),
            calls,
            shared: Arc::clone(&self.shared),
            next_event: Arc::clone(&self.next_event),
            info: info.clone(),
            writer,
            files,
        };
        let handle = std::thread::Builder::new()
            .name(format!("automation-{id}"))
            .spawn(move || engine.run())
            .map_err(|e| OpError::internal(format!("the automation engine didn't start: {e}")))?;
        *run = Some(Running { info: info.clone(), def: def.clone(), inbox, stop, engine: Some(handle) });
        Ok(info)
    }

    /// The running show, if any.
    #[must_use]
    pub fn running(&self) -> Option<RunInfo> {
        lock(&self.run).as_ref().filter(|r| r.engine.as_ref().is_some_and(|h| !h.is_finished())).map(|r| r.info.clone())
    }

    /// Checks `change` against the running show and queues it as a step of its own.
    fn control(&self, change: impl FnOnce(&ShowDef) -> Result<Control, OpError>) -> Result<(), OpError> {
        let run = lock(&self.run);
        let running =
            run.as_ref().filter(|r| r.engine.as_ref().is_some_and(|h| !h.is_finished())).ok_or_else(not_running)?;
        let control = change(&running.def)?;
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
    fn start(&self, show: &ShowId, _by: &Actor) -> Result<RunInfo, OpError> {
        let runner = self.runner().and_then(|w| w.upgrade()).ok_or_else(|| OpError::unavailable("runner"))?;
        let node = runner
            .get(&paths::show(show), &Grants::of([Scope::Read]))?
            .ok_or_else(|| OpError::not_found(format!("there is no show `{show}`")))?;
        let def = ShowDef::from_node(&node).map_err(|e| OpError::invalid("show", e.to_string()))?;
        self.start_def(show, &def)
    }

    fn stop(&self, _by: &Actor) -> Result<(), OpError> {
        let mut running = lock(&self.run).take().ok_or_else(not_running)?;
        running.stop.store(true, Ordering::SeqCst);
        running.inbox.close();
        // The engine thread never waits on the executor, so joining it here (even from inside an op) is safe.
        if let Some(handle) = running.engine.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    fn pause(&self, _by: &Actor) -> Result<(), OpError> {
        self.control(|_| Ok(Control::Pause))
    }

    fn resume(&self, _by: &Actor) -> Result<(), OpError> {
        self.control(|_| Ok(Control::Resume))
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
    calls: Sender<OpRequest>,
    shared: Arc<Shared>,
    next_event: Arc<AtomicU64>,
    info: RunInfo,
    writer: Option<Writer>,
    /// Which file each persisted scope writes to.
    files: Vec<(VarScope, PersistScope)>,
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
                let _ = self.calls.send(call);
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
                self.core.step(self.clock.now(), self.wall.now(), item);
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
        *lock(&self.shared.status) = Status::default()
            .with_running(self.info.clone())
            .with_paused(self.core.paused())
            .with_graphs(self.core.graphs())
            .with_dropped(dropped);
    }
}

/// Runs a show's op calls in order through the runner and posts each reply to the inbox (design D7's ordered lane).
struct Dispatcher {
    runner: Option<Weak<dyn Runner>>,
    inbox: Arc<Inbox>,
}

impl Dispatcher {
    fn run(self, calls: &Receiver<OpRequest>) {
        for call in calls {
            let outcome = self.call(&call);
            self.inbox.push_reply(Item::Reply { call: call.call, outcome });
        }
    }

    fn call(&self, call: &OpRequest) -> Result<Option<Value>, OpError> {
        let runner = self.runner.as_ref().and_then(Weak::upgrade).ok_or_else(|| {
            OpError::unsupported("the automation engine has no op runner")
                .hint("the app attaches its runner at startup")
        })?;
        let request = Request {
            op: call.op.clone(),
            args: call.args.clone(),
            mode: Mode::Apply,
            actor: call.acting.clone().unwrap_or(Actor::System),
            grants: show_grants(),
            cause: Some(Cause { kind: CauseKind::Rule, text: Some(call.cause.clone()) }),
            tx: None,
            base: None,
        };
        runner.run(request).map(|outcome| outcome.plan.output)
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
        let grants = show_grants();
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
