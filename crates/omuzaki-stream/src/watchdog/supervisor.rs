//! The supervisor: runs the [`Monitor`] against real workers on a thread of its own (spec v3 §25, BR-23).
//!
//! Every tick it reads each instance's heartbeat and exit, lets the policy decide, and carries the actions out:
//! spawning through the worker's launcher, killing, holding the outputs (they re-present their last frame while a
//! worker that feeds them is down) and telling subscribers. A restart a person asks for and the stop arrive on a
//! channel, so they act at once rather than on the next tick. `stop` kills every child and reaps it.

use std::collections::{BTreeSet, VecDeque};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::Clock;

use super::launch::{WorkerHandle, WorkerLauncher};
use super::policy::{Action, Health, HealthEvent, Monitor, Observation, RestartRecord, WorkerId, WorkerSpec};

/// The default tick.
pub const DEFAULT_TICK: Duration = Duration::from_millis(50);

/// How long `stop` waits for a killed child to be reaped before leaving it to its handle's drop.
const REAP_FOR: Duration = Duration::from_secs(5);

/// The output host's side of a hold: while on, it re-presents its last frame.
pub trait OutputHold: Send + Sync {
    /// Holds (`true`) or releases (`false`) the outputs.
    fn hold(&self, on: bool);
}

enum Command {
    Restart(WorkerId),
    Stop,
}

/// What the supervisor thread publishes for the handle's readers.
#[derive(Default)]
struct Shared {
    health: Vec<(WorkerId, Health)>,
    pids: Vec<(WorkerId, Option<u32>)>,
    log: Vec<RestartRecord>,
    subscribers: Vec<Sender<HealthEvent>>,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    // A panic elsewhere never makes the health unreadable.
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running supervisor.
pub struct Supervisor {
    commands: Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor").field("health", &self.health()).finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Starts supervising `workers` (worker `i` is `WorkerId(i)`), holding outputs through `hold`, on `clock`, looking
    /// every `tick` ([`DEFAULT_TICK`] is 50 ms). Each worker is spawned at once.
    pub fn start(
        workers: Vec<(WorkerSpec, Box<dyn WorkerLauncher>)>,
        hold: Arc<dyn OutputHold>,
        clock: Arc<dyn Clock>,
        tick: Duration,
    ) -> Supervisor {
        let (specs, launchers): (Vec<_>, Vec<_>) = workers.into_iter().unzip();
        let monitor = Monitor::new(specs, clock.now());
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (commands, inbox) = mpsc::channel();
        let mut runner = Runner {
            handles: launchers.iter().map(|_| None).collect(),
            launchers,
            monitor,
            hold,
            clock,
            shared: Arc::clone(&shared),
            held: BTreeSet::new(),
            out: Vec::with_capacity(16),
            queue: VecDeque::new(),
        };
        runner.poll();
        let thread =
            std::thread::Builder::new().name("watchdog".to_owned()).spawn(move || runner.run(&inbox, tick)).ok();
        Supervisor { commands, shared, thread }
    }

    /// Each worker's health.
    pub fn health(&self) -> Vec<(WorkerId, Health)> {
        lock(&self.shared).health.clone()
    }

    /// Each worker's current process id, for process workers that are running.
    pub fn pids(&self) -> Vec<(WorkerId, Option<u32>)> {
        lock(&self.shared).pids.clone()
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

    /// A person's restart of worker `id`; it also revives a worker that gave up.
    pub fn restart(&self, id: WorkerId) {
        // A send error means the thread has ended, and so has every worker.
        let _ = self.commands.send(Command::Restart(id));
    }

    /// Kills every child, reaps it and ends the supervisor thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The supervisor thread's state.
struct Runner {
    launchers: Vec<Box<dyn WorkerLauncher>>,
    handles: Vec<Option<Box<dyn WorkerHandle>>>,
    monitor: Monitor,
    hold: Arc<dyn OutputHold>,
    clock: Arc<dyn Clock>,
    shared: Arc<Mutex<Shared>>,
    /// The workers holding the outputs.
    held: BTreeSet<WorkerId>,
    out: Vec<Action>,
    queue: VecDeque<Action>,
}

fn worker(i: usize) -> WorkerId {
    WorkerId(u16::try_from(i).unwrap_or(u16::MAX))
}

impl Runner {
    fn run(mut self, inbox: &Receiver<Command>, tick: Duration) {
        let mut next = Instant::now() + tick;
        loop {
            let wait = next.saturating_duration_since(Instant::now());
            match inbox.recv_timeout(wait) {
                Ok(Command::Restart(id)) => {
                    let now = self.clock.now();
                    self.monitor.request_restart(now, id, &mut self.out);
                    self.queue.extend(self.out.drain(..));
                    self.drain();
                    self.publish();
                    continue;
                }
                Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            next += tick;
            self.poll();
        }
        self.reap_all();
    }

    /// Reads every instance, lets the policy decide, acts, and publishes.
    fn poll(&mut self) {
        let now = self.clock.now();
        for i in 0..self.handles.len() {
            let Some(handle) = self.handles[i].as_mut() else { continue };
            let (beat, exit) = (handle.heartbeat(), handle.try_wait());
            self.monitor.observe(now, worker(i), Observation::Beat(beat), &mut self.out);
            self.queue.extend(self.out.drain(..));
            if let Some(exit) = exit {
                self.handles[i] = None;
                self.monitor.observe(now, worker(i), Observation::Exited { code: exit.code }, &mut self.out);
                self.queue.extend(self.out.drain(..));
            }
        }
        self.monitor.tick(now, &mut self.out);
        self.queue.extend(self.out.drain(..));
        self.drain();
        self.publish();
    }

    /// Carries out the queued actions, and any their observations lead to. A release is applied after the health is
    /// published, so the outputs never see "released" while the worker still reads as down.
    fn drain(&mut self) {
        let mut release = false;
        while let Some(action) = self.queue.pop_front() {
            match action {
                Action::Spawn(id) => self.spawn(id),
                Action::Kill(id) => {
                    if let Some(handle) = self.handles.get_mut(usize::from(id.0)).and_then(Option::as_mut) {
                        handle.kill();
                    }
                }
                Action::HoldOutputs(id) => {
                    if self.held.is_empty() {
                        self.hold.hold(true);
                    }
                    self.held.insert(id);
                }
                Action::ReleaseOutputs(id) => {
                    self.held.remove(&id);
                    release |= self.held.is_empty();
                }
                Action::Event(event) => {
                    lock(&self.shared).subscribers.retain(|tx| tx.send(event).is_ok());
                }
            }
        }
        if release {
            self.publish();
            if self.held.is_empty() {
                self.hold.hold(false);
            }
        }
    }

    fn spawn(&mut self, id: WorkerId) {
        let i = usize::from(id.0);
        let (Some(launcher), Some(slot)) = (self.launchers.get(i), self.handles.get(i)) else { return };
        if slot.is_some() {
            return;
        }
        let now = self.clock.now();
        let instance = launcher.spawn(id);
        self.monitor.observe(now, id, Observation::Spawned, &mut self.out);
        self.queue.extend(self.out.drain(..));
        if let Ok(handle) = instance {
            self.handles[i] = Some(handle);
        } else {
            // An instance that never started is one that crashed at once: the policy backs off and gives up as for any.
            self.monitor.observe(now, id, Observation::Exited { code: None }, &mut self.out);
            self.queue.extend(self.out.drain(..));
        }
    }

    fn publish(&self) {
        let mut shared = lock(&self.shared);
        shared.health = (0..self.handles.len()).map(|i| (worker(i), self.monitor.health(worker(i)))).collect();
        shared.pids = self
            .handles
            .iter()
            .enumerate()
            .map(|(i, handle)| (worker(i), handle.as_ref().and_then(|h| h.pid())))
            .collect();
        shared.log = self.monitor.log().records().copied().collect();
    }

    /// Kills every instance and reaps it, giving up on one after [`REAP_FOR`] (its drop reaps it then).
    fn reap_all(&mut self) {
        for handle in self.handles.iter_mut().flatten() {
            handle.kill();
        }
        let deadline = Instant::now() + REAP_FOR;
        for slot in &mut self.handles {
            if let Some(handle) = slot.as_mut() {
                while handle.try_wait().is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            *slot = None;
        }
        self.publish();
    }
}
