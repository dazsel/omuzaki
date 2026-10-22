//! The supervisor: runs the [`Monitor`] against real workers on a thread of its own (spec v3 §25, BR-23;
//! `docs/design/supervisor.md` §4, §5, §15.4).
//!
//! The thread waits on one inbox with the tick as its timeout. Each worker's reader wakes it at once on `Hello` (it
//! sends `Start` and replays the worker's latched state), on a fault (it kills the instance) and when the control
//! channel ends (it reaps the instance and lets the policy decide), so a crash is seen in milliseconds, not at the next
//! tick. Every tick it reads each instance's progress, checks for kills that have not finished (`Stuck`, or a thread
//! `Abandoned` after `exit_wait`), lets the policy decide, and carries the actions out: spawning a new generation,
//! killing, telling the [`Handoff`] that a worker is down or up, and telling subscribers.
//!
//! **State messages are latched; commands are not.** `Scene`, `Source` and `Attach` are kept per worker (the newest
//! of each) and replayed after every `Start`, and accepted while the worker is down. `Frame`, `SlotFree` and `Keyframe`
//! go straight to the live generation's control channel, and are refused with [`SendError::Down`] while it is down.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::Clock;
use omuzaki_stream_api::supervisor::Scope;
use omuzaki_stream_api::worker::{ChannelHandle, ChannelRef, ChannelRole, HostMsg};

use super::launch::{ControlSend, Fault, LaunchError, Wake, WakeKind, Waker, WorkerHandle, WorkerLauncher};
use super::policy::{
    Action, HealthEvent, Monitor, Observation, RestartRecord, UnknownWorker, WorkerId, WorkerKind, WorkerSpec,
    WorkerStatus,
};

/// The supervisor's timing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// Which supervisor this is.
    pub scope: Scope,
    /// How often deadlines are checked when nothing wakes the thread.
    pub tick: Duration,
    /// How long a kill may take before it is reported `Stuck` (a process) or the thread is abandoned.
    pub exit_wait: Duration,
    /// How long `stop` waits for workers to exit on `Stop` before killing them.
    pub stop_wait: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            scope: Scope::Host,
            tick: Duration::from_millis(50),
            exit_wait: Duration::from_secs(1),
            stop_wait: Duration::from_secs(2),
        }
    }
}

/// What the rest of the show does while a worker is down: hold the last frame, hand the face to the life layer, mark
/// an audio gap. Called on the supervisor thread: never blocks.
pub trait Handoff: Send + Sync {
    /// Worker `id` stopped serving.
    fn down(&self, id: WorkerId, kind: WorkerKind);
    /// It serves again.
    fn up(&self, id: WorkerId, kind: WorkerKind);
}

/// A handoff that does nothing.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoHandoff;

impl Handoff for NoHandoff {
    fn down(&self, _: WorkerId, _: WorkerKind) {}
    fn up(&self, _: WorkerId, _: WorkerKind) {}
}

/// Why a message was not sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SendError {
    /// No such worker.
    #[error(transparent)]
    Unknown(#[from] UnknownWorker),
    /// A command to a worker that is down (or not started yet): it would not understand it.
    #[error("the worker is down")]
    Down,
    /// An `Attach` without its object: use [`Supervisor::attach`].
    #[error("an Attach needs its object: use attach")]
    NeedsObject,
}

/// What `stop` left behind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopReport {
    /// Workers whose process was still alive after the kill and `exit_wait`: their handles are closed and they are
    /// left to the OS (the end of the control channel and the parent-death signal end them).
    pub stuck: Vec<WorkerId>,
}

enum Inbox {
    Wake(Wake),
    Restart(WorkerId),
    Stop,
}

/// One worker's latched state.
#[derive(Default)]
struct Latches {
    scene: Option<HostMsg>,
    sources: Vec<(WorkerId, HostMsg)>,
    attaches: Vec<(ChannelRole, HostMsg, ChannelHandle)>,
}

impl Latches {
    fn replay(&self, control: &dyn ControlSend) -> Result<(), LaunchError> {
        if let Some(scene) = &self.scene {
            control.send(scene, &[])?;
        }
        for (_, msg) in &self.sources {
            control.send(msg, &[])?;
        }
        for (_, msg, handle) in &self.attaches {
            // A fresh clone each time: the latch keeps its own copy.
            let clone = handle.try_clone().map_err(|e| LaunchError::spawn(e.to_string()))?;
            control.send(msg, &[&clone])?;
        }
        Ok(())
    }
}

/// What the thread publishes, and what callers write through.
struct Shared {
    status: Vec<WorkerStatus>,
    log: Vec<RestartRecord>,
    subscribers: Vec<Sender<HealthEvent>>,
    /// Per worker: the live generation and its control channel, once `Start` has gone and while it is up.
    live: Vec<Option<(u32, Arc<dyn ControlSend>)>>,
    latches: Vec<Latches>,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    // A panic elsewhere never makes the status unreadable.
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running supervisor.
pub struct Supervisor {
    inbox: Sender<Inbox>,
    shared: Arc<Mutex<Shared>>,
    ids: Vec<WorkerId>,
    thread: Option<JoinHandle<StopReport>>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor").field("ids", &self.ids).finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Starts supervising `workers` (in [`ids`](Self::ids) order), telling `handoff` when one is down or up, on
    /// `clock` (the show clock). Each worker is spawned at once.
    ///
    /// # Errors
    /// [`LaunchError::Permanent`] when a spec's kind and its launcher's disagree; [`LaunchError::Spawn`] when the
    /// supervisor thread cannot start.
    pub fn start(
        workers: Vec<(WorkerSpec, Box<dyn WorkerLauncher>)>,
        handoff: Arc<dyn Handoff>,
        clock: Arc<dyn Clock>,
        cfg: SupervisorConfig,
    ) -> Result<Supervisor, LaunchError> {
        for (spec, launcher) in &workers {
            if spec.kind != launcher.kind() {
                return Err(LaunchError::permanent(format!(
                    "worker {} is a {:?} but its launcher starts a {:?}",
                    spec.name,
                    spec.kind,
                    launcher.kind()
                )));
            }
        }
        let (specs, launchers): (Vec<_>, Vec<_>) = workers.into_iter().unzip();
        let monitor = Monitor::new(cfg.scope, specs, clock.now());
        let ids: Vec<WorkerId> = monitor.ids().collect();
        let status = ids.iter().filter_map(|id| monitor.status(*id).ok()).collect();
        let shared = Arc::new(Mutex::new(Shared {
            status,
            log: Vec::new(),
            subscribers: Vec::new(),
            live: ids.iter().map(|_| None).collect(),
            latches: ids.iter().map(|_| Latches::default()).collect(),
        }));
        let (inbox, rx) = mpsc::channel();
        let wake_tx = Mutex::new(inbox.clone());
        let waker = Waker::new(move |wake| {
            // A send error means the supervisor has ended; nothing is listening.
            let _ = wake_tx.lock().unwrap_or_else(PoisonError::into_inner).send(Inbox::Wake(wake));
        });
        let mut runner = Runner {
            slots: launchers
                .into_iter()
                .map(|launcher| Slot { launcher, handle: None, generation: 0, started: false, killed_at: None })
                .collect(),
            ids: ids.clone(),
            monitor,
            handoff,
            clock,
            cfg,
            shared: Arc::clone(&shared),
            waker,
            out: Vec::with_capacity(16),
            queue: VecDeque::new(),
        };
        // Every spawn happens on the supervisor thread, the first ones too: on Linux a worker's parent-death signal
        // fires when the *thread* that spawned it ends, and the caller's thread may end long before the show does.
        let thread = std::thread::Builder::new()
            .name(format!("supervisor-{:?}", cfg.scope).to_lowercase())
            .spawn(move || {
                runner.poll();
                runner.run(&rx)
            })
            .map_err(|e| LaunchError::spawn(e.to_string()))?;
        Ok(Supervisor { inbox, shared, ids, thread: Some(thread) })
    }

    /// Every worker's id, in the order of the specs given to `start`.
    pub fn ids(&self) -> &[WorkerId] {
        &self.ids
    }

    fn index(&self, id: WorkerId) -> Result<usize, UnknownWorker> {
        self.ids.iter().position(|w| *w == id).ok_or(UnknownWorker { id })
    }

    /// Every worker's health-meter row.
    pub fn status(&self) -> Vec<WorkerStatus> {
        lock(&self.shared).status.clone()
    }

    /// The restart log, oldest first.
    pub fn log(&self) -> Vec<RestartRecord> {
        lock(&self.shared).log.clone()
    }

    /// A new subscription to health events, from now on.
    pub fn events(&self) -> Receiver<HealthEvent> {
        let (tx, rx) = mpsc::channel();
        lock(&self.shared).subscribers.push(tx);
        rx
    }

    /// Sends `msg` to worker `id`. State messages (`Scene`, `Source`) are latched and replayed after every `Start`,
    /// and accepted while the worker is down; commands (`Frame`, `SlotFree`, `Keyframe`, `Stop`) go to the live
    /// generation at once.
    ///
    /// # Errors
    /// [`SendError::Unknown`] for an id that is not this supervisor's; [`SendError::Down`] for a command while the
    /// worker is down; [`SendError::NeedsObject`] for an `Attach` (use [`attach`](Self::attach)).
    pub fn send(&self, id: WorkerId, msg: &HostMsg) -> Result<(), SendError> {
        let i = self.index(id)?;
        let mut shared = lock(&self.shared);
        match msg {
            HostMsg::Attach { .. } => return Err(SendError::NeedsObject),
            HostMsg::Scene { .. } => shared.latches[i].scene = Some(msg.clone()),
            HostMsg::Source { source, .. } => {
                let source = *source;
                let sources = &mut shared.latches[i].sources;
                match sources.iter_mut().find(|(s, _)| *s == source) {
                    Some(entry) => entry.1 = msg.clone(),
                    None => sources.push((source, msg.clone())),
                }
            }
            _ => {
                let (_, control) = shared.live[i].clone().ok_or(SendError::Down)?;
                drop(shared);
                return control.send(msg, &[]).map_err(|_| SendError::Down);
            }
        }
        if let Some((_, control)) = shared.live[i].clone() {
            // A failure means the generation is ending; the latch reaches the next one.
            let _ = control.send(msg, &[]);
        }
        Ok(())
    }

    /// Tells worker `id` that the peer channel `role` is now `object`, of the peer's `generation`. Latched like any
    /// state message: the latch owns `object` and every replay sends a fresh clone of it.
    ///
    /// # Errors
    /// [`SendError::Unknown`] for an id that is not this supervisor's.
    pub fn attach(
        &self,
        id: WorkerId,
        role: ChannelRole,
        generation: u32,
        object: ChannelHandle,
    ) -> Result<(), SendError> {
        let i = self.index(id)?;
        #[cfg(windows)]
        let channel = ChannelRef::name(object.name());
        #[cfg(not(windows))]
        let channel = ChannelRef::fd(0);
        let msg = HostMsg::attach(role, channel, generation);
        let mut shared = lock(&self.shared);
        let attaches = &mut shared.latches[i].attaches;
        attaches.retain(|(r, _, _)| *r != role);
        attaches.push((role, msg.clone(), object));
        if let Some((_, control)) = shared.live[i].clone()
            && let Some((_, _, handle)) = shared.latches[i].attaches.last()
            && let Ok(clone) = handle.try_clone()
        {
            let _ = control.send(&msg, &[&clone]);
        }
        Ok(())
    }

    /// The generation of worker `id` that is up and serving, if any.
    ///
    /// # Errors
    /// [`UnknownWorker`] for an id that is not this supervisor's.
    pub fn live_generation(&self, id: WorkerId) -> Result<Option<u32>, UnknownWorker> {
        let i = self.index(id)?;
        Ok(lock(&self.shared).live[i].as_ref().map(|(generation, _)| *generation))
    }

    /// Sends a command to `generation` of worker `id` only: a caller holding one generation's shared objects (the
    /// remote encoder's slots) never reaches the next generation by mistake.
    ///
    /// # Errors
    /// [`SendError::Unknown`] for an id that is not this supervisor's; [`SendError::Down`] when that generation is not
    /// the live one (it has ended, or the worker is down); [`SendError::NeedsObject`] for an `Attach`.
    pub fn send_to(&self, id: WorkerId, generation: u32, msg: &HostMsg) -> Result<(), SendError> {
        let i = self.index(id)?;
        if matches!(msg, HostMsg::Attach { .. }) {
            return Err(SendError::NeedsObject);
        }
        let control = match &lock(&self.shared).live[i] {
            Some((live, control)) if *live == generation => Arc::clone(control),
            _ => return Err(SendError::Down),
        };
        control.send(msg, &[]).map_err(|_| SendError::Down)
    }

    /// A person's restart of worker `id`; it also revives a worker that gave up.
    ///
    /// # Errors
    /// [`UnknownWorker`] for an id that is not this supervisor's.
    pub fn restart(&self, id: WorkerId) -> Result<(), UnknownWorker> {
        self.index(id)?;
        // A send error means the thread has ended, and so has every worker.
        let _ = self.inbox.send(Inbox::Restart(id));
        Ok(())
    }

    /// Sends `Stop` to every worker, kills those still running after `stop_wait`, then waits at most `exit_wait` more.
    /// Never blocks longer than `stop_wait + exit_wait` (plus a tick).
    pub fn stop(mut self) -> StopReport {
        self.shutdown()
    }

    fn shutdown(&mut self) -> StopReport {
        let _ = self.inbox.send(Inbox::Stop);
        self.thread.take().and_then(|thread| thread.join().ok()).unwrap_or_default()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// One worker on the supervisor thread.
struct Slot {
    launcher: Box<dyn WorkerLauncher>,
    handle: Option<Box<dyn WorkerHandle>>,
    generation: u32,
    /// `Start` has gone to the current generation.
    started: bool,
    /// When the current instance was killed.
    killed_at: Option<Instant>,
}

/// The supervisor thread's state.
struct Runner {
    slots: Vec<Slot>,
    ids: Vec<WorkerId>,
    monitor: Monitor,
    handoff: Arc<dyn Handoff>,
    clock: Arc<dyn Clock>,
    cfg: SupervisorConfig,
    shared: Arc<Mutex<Shared>>,
    waker: Waker,
    out: Vec<Action>,
    queue: VecDeque<Action>,
}

/// How soon to look again after a channel ended but its process was not reaped yet.
const RECHECK: Duration = Duration::from_millis(2);

impl Runner {
    fn run(mut self, inbox: &Receiver<Inbox>) -> StopReport {
        let mut next = Instant::now() + self.cfg.tick;
        let mut recheck: Option<Instant> = None;
        loop {
            let until = recheck.map_or(next, |r| r.min(next));
            match inbox.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Inbox::Wake(wake)) => {
                    if self.wake(wake) {
                        recheck = Some(Instant::now() + RECHECK);
                    }
                    continue;
                }
                Ok(Inbox::Restart(id)) => {
                    let now = self.clock.now();
                    if self.monitor.request_restart(now, id, &mut self.out).is_ok() {
                        self.queue.extend(self.out.drain(..));
                        self.drain();
                        self.publish();
                    }
                    continue;
                }
                Ok(Inbox::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if Instant::now() >= next {
                next += self.cfg.tick;
                if next < Instant::now() {
                    next = Instant::now() + self.cfg.tick;
                }
            }
            recheck = None;
            if self.poll() {
                recheck = Some(Instant::now() + RECHECK);
            }
        }
        self.stop_all()
    }

    /// Handles a reader's wake-up; `true` when an ended instance is not reaped yet (look again soon).
    fn wake(&mut self, wake: Wake) -> bool {
        let Ok(i) = self.monitor.index(wake.id) else { return false };
        if self.slots[i].generation != wake.generation || self.slots[i].handle.is_none() {
            // A wake from a generation that has already ended: nothing to do.
            return false;
        }
        match wake.kind {
            WakeKind::Hello => self.start_generation(i),
            WakeKind::Fault => {
                self.close(i);
                self.kill(i);
            }
            _ => {}
        }
        self.poll()
    }

    /// Sends `Start`, replays the latched state and opens the worker to commands.
    fn start_generation(&mut self, i: usize) {
        let slot = &mut self.slots[i];
        let generation = slot.generation;
        let Some(handle) = slot.handle.as_mut() else { return };
        if slot.started {
            return;
        }
        slot.started = true;
        if handle.start().is_err() {
            // The channel closed: its end will be observed.
            return;
        }
        let control = handle.control();
        let mut shared = lock(&self.shared);
        if shared.latches[i].replay(control.as_ref()).is_ok() {
            shared.live[i] = Some((generation, control));
        }
    }

    /// Stops accepting commands for worker `i`.
    fn close(&self, i: usize) {
        lock(&self.shared).live[i] = None;
    }

    fn kill(&mut self, i: usize) {
        let slot = &mut self.slots[i];
        if let Some(handle) = slot.handle.as_mut() {
            handle.kill();
            slot.killed_at.get_or_insert_with(Instant::now);
        }
    }

    fn observe(&mut self, i: usize, obs: Observation) {
        let now = self.clock.now();
        if self.monitor.observe(now, self.ids[i], obs, &mut self.out).is_ok() {
            self.queue.extend(self.out.drain(..));
        }
    }

    /// Reads every instance, lets the policy decide, acts, and publishes; `true` when an instance whose channel ended
    /// has not been reaped yet.
    fn poll(&mut self) -> bool {
        let mut pending = false;
        for i in 0..self.slots.len() {
            let Some(handle) = self.slots[i].handle.as_mut() else { continue };
            let beat = handle.heartbeat().read();
            let exit = handle.try_wait();
            let fault = handle.fault();
            let revocable = handle.revocable();
            self.observe(i, Observation::Beat(beat));
            if let Some(exit) = exit {
                self.slots[i].handle = None;
                self.slots[i].killed_at = None;
                self.close(i);
                let obs = match fault {
                    Some(Fault::Protocol(what)) => Observation::Faulted { what },
                    Some(Fault::Launch { what, permanent }) => Observation::LaunchFailed { what, permanent },
                    _ => Observation::Exited { code: exit.code },
                };
                self.observe(i, obs);
                continue;
            }
            if fault.is_some() {
                // A fault the reader reported but whose wake was missed: kill it now.
                self.close(i);
                self.kill(i);
            }
            if let Some(at) = self.slots[i].killed_at {
                pending = true;
                if at.elapsed() >= self.cfg.exit_wait {
                    if revocable {
                        // A thread can't be killed: abandon it (its objects are quarantined) and spawn anew.
                        self.slots[i].handle = None;
                        self.slots[i].killed_at = None;
                        self.observe(i, Observation::Abandoned);
                    } else {
                        self.observe(i, Observation::Stuck);
                    }
                }
            }
        }
        let now = self.clock.now();
        self.monitor.tick(now, &mut self.out);
        self.queue.extend(self.out.drain(..));
        self.drain();
        self.publish();
        pending
    }

    /// Carries out the queued actions, and any their observations lead to.
    fn drain(&mut self) {
        while let Some(action) = self.queue.pop_front() {
            match action {
                Action::Spawn(id) => self.spawn(id),
                Action::Kill(id) => {
                    if let Ok(i) = self.monitor.index(id) {
                        self.close(i);
                        self.kill(i);
                    }
                }
                Action::Down(id) => {
                    if let Ok(i) = self.monitor.index(id) {
                        self.close(i);
                        self.handoff.down(id, self.slots[i].launcher.kind());
                    }
                }
                Action::Up(id) => {
                    if let Ok(i) = self.monitor.index(id) {
                        self.handoff.up(id, self.slots[i].launcher.kind());
                    }
                }
                Action::Event(event) => {
                    lock(&self.shared).subscribers.retain(|tx| tx.send(event.clone()).is_ok());
                }
            }
        }
    }

    fn spawn(&mut self, id: WorkerId) {
        let Ok(i) = self.monitor.index(id) else { return };
        if self.slots[i].handle.is_some() {
            return;
        }
        let slot = &mut self.slots[i];
        slot.generation = slot.generation.wrapping_add(1);
        slot.started = false;
        slot.killed_at = None;
        match slot.launcher.spawn(id, slot.generation, self.waker.clone()) {
            Ok(handle) => {
                let needs_hello = handle.needs_hello();
                slot.handle = Some(handle);
                self.observe(i, Observation::Spawned);
                if !needs_hello {
                    self.start_generation(i);
                }
            }
            Err(e) => {
                let permanent = e.is_permanent();
                self.observe(i, Observation::LaunchFailed { what: e.to_string(), permanent });
            }
        }
    }

    fn publish(&self) {
        let mut shared = lock(&self.shared);
        shared.status = self
            .ids
            .iter()
            .zip(&self.slots)
            .filter_map(|(id, slot)| {
                let mut row = self.monitor.status(*id).ok()?;
                if let Some(handle) = &slot.handle {
                    row.pid = handle.pid();
                    row.work = handle.heartbeat().work();
                }
                Some(row)
            })
            .collect();
        shared.log = self.monitor.log().iter().cloned().collect();
    }

    /// `Stop` to every worker, a kill after `stop_wait`, then at most `exit_wait` more; reports the processes still
    /// alive.
    fn stop_all(&mut self) -> StopReport {
        {
            let mut shared = lock(&self.shared);
            for live in &mut shared.live {
                if let Some((_, control)) = live.take() {
                    let _ = control.send(&HostMsg::Stop, &[]);
                }
            }
        }
        for slot in &mut self.slots {
            if let Some(handle) = slot.handle.as_mut()
                && !handle.revocable()
                && !slot.started
            {
                // Not started yet: it would not understand `Stop`.
                handle.kill();
            }
        }
        let wait_all = |slots: &mut [Slot], until: Instant| {
            while Instant::now() < until {
                if slots.iter_mut().all(|s| s.handle.as_mut().is_none_or(|h| h.try_wait().is_some())) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        wait_all(&mut self.slots, Instant::now() + self.cfg.stop_wait);
        for slot in &mut self.slots {
            if let Some(handle) = slot.handle.as_mut()
                && handle.try_wait().is_none()
            {
                handle.kill();
            }
        }
        wait_all(&mut self.slots, Instant::now() + self.cfg.exit_wait);
        let mut report = StopReport::default();
        for (id, slot) in self.ids.iter().zip(&mut self.slots) {
            if let Some(mut handle) = slot.handle.take()
                && handle.try_wait().is_none()
                && !handle.revocable()
            {
                report.stuck.push(*id);
            }
        }
        self.publish();
        report
    }
}
