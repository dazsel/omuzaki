//! The watchdog policy: when to restart, how often, and when to give up (spec v3 §25, BR-23).
//!
//! A supervisor's decisions as a pure state machine: time and observations in, actions out. No processes, threads or
//! clock reads (`now` is a parameter), so every case is deterministic. [`Monitor::observe`] and [`Monitor::tick`]
//! reuse the caller's `Vec` and allocate nothing once it has capacity; [`HeartbeatCell::beat`] is one relaxed atomic
//! add.
//!
//! The rules:
//! - **Starting** from `Spawned` until the first beat that advances the counter, or until `start_grace` passes without
//!   one (then `Hung { silent_for: start_grace }`). **Healthy** on that first advance.
//! - **Hung:** while Healthy, `now - last_advance >= hung_after`. **Crashed:** `Exited` while Starting, Restarting or
//!   Healthy.
//! - On either: `HoldOutputs` (if the spec holds outputs and they aren't held yet), `Kill` for a hang, and
//!   `Event(Restarted)`. `Spawn` comes only after the `Exited` (never two instances): at once for attempt 1, after the
//!   backoff otherwise.
//! - **Backoff** for attempt `n` within the `within` window: 0 for `n = 1`, `min(backoff_first × 2^(n − 2),
//!   backoff_max)` after. A restart past `max_restarts` makes the worker `Failed` with one `GaveUp`, and nothing
//!   spawns until [`Monitor::request_restart`].
//! - **Stable:** `stable_after` of continuous Healthy resets the attempts and the window.
//! - **Recovered:** the first advancing beat after a respawn releases held outputs and reports the downtime since the
//!   first failure, `slow` past 2 s.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_core::time::Seconds;

/// The records the restart log keeps.
pub const LOG_CAPACITY: usize = 256;

/// The downtime past which a recovery counts as slow (BR-23's bound).
pub const SLOW_RECOVERY: Seconds = Seconds(2.0);

/// Slack for comparing sums of seconds against deadlines.
const EPSILON: f64 = 1e-9;

/// A supervised worker: its index in the monitor's specs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkerId(pub u16);

/// What a worker does, for logs and the health meter.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorkerKind {
    /// The renderer.
    Renderer,
    /// The face tracker.
    Tracker,
    /// The audio engine.
    Audio,
    /// An encoder.
    Encoder,
    /// A platform adapter.
    Adapter,
}

/// How a worker is restarted.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RestartPolicy {
    /// The wait before the second attempt; it doubles per attempt after.
    pub backoff_first: Seconds,
    /// The longest wait.
    pub backoff_max: Seconds,
    /// The most restarts inside `within` before giving up.
    pub max_restarts: u32,
    /// The window restarts are counted in.
    pub within: Seconds,
    /// Continuous health that forgives earlier restarts.
    pub stable_after: Seconds,
}

/// One supervised worker.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkerSpec {
    /// Its name, for logs.
    pub name: String,
    /// What it does.
    pub kind: WorkerKind,
    /// Whether outputs re-present their last frame while it is down.
    pub hold_outputs: bool,
    /// How long a fresh instance may take to beat for the first time.
    pub start_grace: Seconds,
    /// How long a healthy worker may stay silent.
    pub hung_after: Seconds,
    /// How it is restarted.
    pub restart: RestartPolicy,
}

/// A worker's health.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Health {
    /// Spawned (or about to be) and not beating yet.
    Starting,
    /// Beating.
    Healthy,
    /// Being replaced: the old instance is dying or the new one hasn't beaten yet.
    Restarting {
        /// The attempt within the window, from 1.
        attempt: u32,
    },
    /// Waiting before the next spawn.
    Backoff {
        /// The attempt within the window, from 1.
        attempt: u32,
        /// When it spawns.
        until: Seconds,
    },
    /// Out of restarts; it stays down until a person restarts it.
    Failed {
        /// The restarts made in the window.
        restarts: u32,
    },
}

/// Why a worker was restarted.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RestartReason {
    /// It ended on its own.
    Crashed {
        /// Its exit code; `None` when a signal killed it.
        exit_code: Option<i32>,
    },
    /// It stopped beating.
    Hung {
        /// How long it had been silent.
        silent_for: Seconds,
    },
    /// A person asked.
    Requested,
}

/// The worker side of a heartbeat: `beat` is wait-free; the monitor side `read`s the counter.
#[derive(Clone, Debug, Default)]
pub struct HeartbeatCell(Arc<AtomicU64>);

impl HeartbeatCell {
    /// A counter at 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// One beat: a relaxed atomic add.
    pub fn beat(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// The beats so far.
    pub fn read(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// What the supervisor saw happen to a worker.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    /// An instance started.
    Spawned,
    /// The instance's heartbeat counter.
    Beat(u64),
    /// The instance ended.
    Exited {
        /// Its exit code; `None` when a signal killed it.
        code: Option<i32>,
    },
}

/// What the supervisor should do.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Action {
    /// Start an instance.
    Spawn(WorkerId),
    /// Kill the instance (never wait for it).
    Kill(WorkerId),
    /// Outputs re-present their last frame.
    HoldOutputs(WorkerId),
    /// Outputs show live frames again.
    ReleaseOutputs(WorkerId),
    /// Tell the person.
    Event(HealthEvent),
}

/// A change of health worth telling.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum HealthEvent {
    /// A worker is being restarted.
    Restarted {
        /// Which.
        worker: WorkerId,
        /// Why.
        reason: RestartReason,
        /// The attempt within the window, from 1.
        attempt: u32,
    },
    /// A restarted worker beats again.
    Recovered {
        /// Which.
        worker: WorkerId,
        /// From the first failure to the first beat.
        downtime: Seconds,
        /// Past [`SLOW_RECOVERY`].
        slow: bool,
    },
    /// A worker ran out of restarts.
    GaveUp {
        /// Which.
        worker: WorkerId,
        /// The restarts made in the window.
        restarts: u32,
    },
}

/// One restart, for the log.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RestartRecord {
    /// When it failed.
    pub at: Seconds,
    /// Which worker.
    pub worker: WorkerId,
    /// Why.
    pub reason: RestartReason,
    /// The attempt within the window, from 1.
    pub attempt: u32,
    /// From the first failure to the first beat after it, once it recovered.
    pub downtime: Option<Seconds>,
}

/// The newest [`LOG_CAPACITY`] restarts.
#[derive(Clone, Debug)]
pub struct RestartLog {
    records: VecDeque<RestartRecord>,
    /// Records ever pushed; record `n` sits at `n - (pushed - len)` while it is kept.
    pushed: u64,
}

impl RestartLog {
    fn new() -> Self {
        Self { records: VecDeque::with_capacity(LOG_CAPACITY), pushed: 0 }
    }

    /// Appends `record`, dropping the oldest past the capacity; returns its sequence number.
    fn push(&mut self, record: RestartRecord) -> u64 {
        if self.records.len() == LOG_CAPACITY {
            self.records.pop_front();
        }
        self.records.push_back(record);
        self.pushed += 1;
        self.pushed - 1
    }

    fn get_mut(&mut self, seq: u64) -> Option<&mut RestartRecord> {
        let first = self.pushed - self.records.len() as u64;
        let at = usize::try_from(seq.checked_sub(first)?).ok()?;
        self.records.get_mut(at)
    }

    /// The records, oldest first.
    pub fn records(&self) -> impl ExactSizeIterator<Item = &RestartRecord> {
        self.records.iter()
    }

    /// How many it holds.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// One worker's state.
// Four independent facts about the instance and the outputs; an enum of their combinations would be harder to read.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct Slot {
    spec: WorkerSpec,
    health: Health,
    /// An instance is running (spawned and not yet exited).
    alive: bool,
    /// A `Spawn` was issued and its `Spawned` hasn't come.
    spawning: bool,
    /// The instance was killed; the next `Exited` is expected, not a crash.
    killed: bool,
    /// When the next spawn is due: while backing off, or once the killed instance has exited.
    spawn_due: Option<Seconds>,
    started_at: Seconds,
    last_beat: u64,
    last_advance: Seconds,
    healthy_since: Option<Seconds>,
    /// The first failure not yet recovered from.
    failed_at: Option<Seconds>,
    /// Restarts within the window.
    attempts: u32,
    window_start: Option<Seconds>,
    held: bool,
    /// The log record a recovery fills.
    record: Option<u64>,
}

impl Slot {
    fn new(spec: WorkerSpec, now: Seconds) -> Self {
        Self {
            spec,
            health: Health::Starting,
            alive: false,
            spawning: false,
            killed: false,
            spawn_due: Some(now),
            started_at: now,
            last_beat: 0,
            last_advance: now,
            healthy_since: None,
            failed_at: None,
            attempts: 0,
            window_start: None,
            held: false,
            record: None,
        }
    }

    /// Forgets earlier restarts after `stable_after` of health or once the window has passed.
    fn settle(&mut self, now: Seconds) {
        let policy = self.spec.restart;
        let stable = self.healthy_since.is_some_and(|since| now.0 - since.0 + EPSILON >= policy.stable_after.0);
        let expired = self.window_start.is_some_and(|start| now.0 - start.0 + EPSILON >= policy.within.0);
        if stable || expired {
            self.attempts = 0;
            self.window_start = None;
        }
    }

    /// The wait before attempt `n`.
    fn backoff(&self, attempt: u32) -> f64 {
        let policy = self.spec.restart;
        if attempt <= 1 {
            return 0.0;
        }
        let doublings = i32::try_from(attempt - 2).unwrap_or(i32::MAX).min(1_000);
        (policy.backoff_first.0 * 2f64.powi(doublings)).min(policy.backoff_max.0)
    }
}

/// The policy for a set of workers.
#[derive(Debug)]
pub struct Monitor {
    slots: Vec<Slot>,
    log: RestartLog,
}

impl Monitor {
    /// Watches `specs` (worker `i` is `WorkerId(i)`); each is due to spawn at `now`.
    pub fn new(specs: Vec<WorkerSpec>, now: Seconds) -> Self {
        Self { slots: specs.into_iter().map(|spec| Slot::new(spec, now)).collect(), log: RestartLog::new() }
    }

    /// How many workers it watches.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether it watches none.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The spec of worker `id`.
    pub fn spec(&self, id: WorkerId) -> Option<&WorkerSpec> {
        self.slots.get(usize::from(id.0)).map(|slot| &slot.spec)
    }

    /// Takes in what happened to worker `id` at `now`; `out` is cleared first and gets the actions.
    pub fn observe(&mut self, now: Seconds, id: WorkerId, obs: Observation, out: &mut Vec<Action>) {
        out.clear();
        let Some(slot) = self.slots.get_mut(usize::from(id.0)) else { return };
        match obs {
            Observation::Spawned => {
                if slot.alive {
                    return;
                }
                (slot.alive, slot.spawning, slot.killed, slot.spawn_due) = (true, false, false, None);
                (slot.started_at, slot.last_beat, slot.last_advance) = (now, 0, now);
            }
            Observation::Beat(count) => {
                if !slot.alive || slot.killed || count <= slot.last_beat {
                    return;
                }
                (slot.last_beat, slot.last_advance) = (count, now);
                if matches!(slot.health, Health::Starting | Health::Restarting { .. }) {
                    slot.health = Health::Healthy;
                    slot.healthy_since = Some(now);
                    if let Some(failed_at) = slot.failed_at.take() {
                        if slot.held {
                            slot.held = false;
                            out.push(Action::ReleaseOutputs(id));
                        }
                        let downtime = Seconds((now.0 - failed_at.0).max(0.0));
                        let past_bound = downtime.0 > SLOW_RECOVERY.0;
                        out.push(Action::Event(HealthEvent::Recovered { worker: id, downtime, slow: past_bound }));
                        if let Some(record) = slot.record.take().and_then(|seq| self.log.get_mut(seq)) {
                            record.downtime = Some(downtime);
                        }
                    }
                }
            }
            Observation::Exited { code } => {
                if !slot.alive {
                    return;
                }
                slot.alive = false;
                if slot.killed {
                    slot.killed = false;
                    Self::spawn_if_due(slot, id, now, out);
                } else if matches!(slot.health, Health::Starting | Health::Healthy | Health::Restarting { .. }) {
                    let reason = RestartReason::Crashed { exit_code: code };
                    Self::fail(slot, &mut self.log, id, now, reason, out);
                }
            }
        }
    }

    /// Checks deadlines and due spawns at `now`; `out` is cleared first and gets the actions.
    pub fn tick(&mut self, now: Seconds, out: &mut Vec<Action>) {
        out.clear();
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let id = WorkerId(u16::try_from(i).unwrap_or(u16::MAX));
            if !slot.alive {
                Self::spawn_if_due(slot, id, now, out);
                continue;
            }
            if slot.killed {
                continue;
            }
            match slot.health {
                Health::Starting | Health::Restarting { .. }
                    if now.0 - slot.started_at.0 + EPSILON >= slot.spec.start_grace.0 =>
                {
                    let reason = RestartReason::Hung { silent_for: slot.spec.start_grace };
                    Self::fail(slot, &mut self.log, id, now, reason, out);
                }
                Health::Healthy if now.0 - slot.last_advance.0 + EPSILON >= slot.spec.hung_after.0 => {
                    let reason = RestartReason::Hung { silent_for: Seconds(now.0 - slot.last_advance.0) };
                    Self::fail(slot, &mut self.log, id, now, reason, out);
                }
                Health::Healthy => slot.settle(now),
                _ => {}
            }
        }
    }

    /// A person's restart of worker `id` at `now`: clears the window, and revives a failed worker. `out` is cleared
    /// first and gets the actions.
    pub fn request_restart(&mut self, now: Seconds, id: WorkerId, out: &mut Vec<Action>) {
        out.clear();
        let Some(slot) = self.slots.get_mut(usize::from(id.0)) else { return };
        (slot.attempts, slot.window_start) = (1, Some(now));
        slot.failed_at.get_or_insert(now);
        slot.record = Some(self.log.push(RestartRecord {
            at: now,
            worker: id,
            reason: RestartReason::Requested,
            attempt: 1,
            downtime: None,
        }));
        slot.health = Health::Restarting { attempt: 1 };
        slot.healthy_since = None;
        slot.spawn_due = Some(now);
        if !slot.alive {
            Self::spawn_if_due(slot, id, now, out);
        } else if !slot.killed {
            if slot.spec.hold_outputs && !slot.held {
                slot.held = true;
                out.push(Action::HoldOutputs(id));
            }
            slot.killed = true;
            out.push(Action::Kill(id));
            let event = HealthEvent::Restarted { worker: id, reason: RestartReason::Requested, attempt: 1 };
            out.push(Action::Event(event));
        }
    }

    /// Worker `id`'s health.
    pub fn health(&self, id: WorkerId) -> Health {
        self.slots.get(usize::from(id.0)).map_or(Health::Failed { restarts: 0 }, |slot| slot.health)
    }

    /// The restart log.
    pub fn log(&self) -> &RestartLog {
        &self.log
    }

    /// Spawns the worker if no instance runs, none is being spawned, it isn't failed and its spawn is due.
    fn spawn_if_due(slot: &mut Slot, id: WorkerId, now: Seconds, out: &mut Vec<Action>) {
        if slot.alive || slot.spawning || matches!(slot.health, Health::Failed { .. }) {
            return;
        }
        let Some(due) = slot.spawn_due else { return };
        if now.0 + EPSILON < due.0 {
            if let Health::Restarting { attempt } = slot.health {
                slot.health = Health::Backoff { attempt, until: due };
            }
            return;
        }
        slot.spawn_due = None;
        slot.spawning = true;
        if let Health::Backoff { attempt, .. } = slot.health {
            slot.health = Health::Restarting { attempt };
        }
        out.push(Action::Spawn(id));
    }

    /// The instance crashed or hung at `now`: count the restart, hold, kill a live one, and spawn or back off.
    fn fail(
        slot: &mut Slot,
        log: &mut RestartLog,
        id: WorkerId,
        now: Seconds,
        reason: RestartReason,
        out: &mut Vec<Action>,
    ) {
        slot.settle(now);
        slot.healthy_since = None;
        let hung = slot.alive;
        if slot.spec.hold_outputs && !slot.held {
            slot.held = true;
            out.push(Action::HoldOutputs(id));
        }
        if hung {
            slot.killed = true;
            out.push(Action::Kill(id));
        }
        let attempt = slot.attempts + 1;
        if attempt > slot.spec.restart.max_restarts {
            slot.health = Health::Failed { restarts: slot.attempts };
            slot.spawn_due = None;
            out.push(Action::Event(HealthEvent::GaveUp { worker: id, restarts: slot.attempts }));
            return;
        }
        slot.attempts = attempt;
        slot.window_start.get_or_insert(now);
        slot.failed_at.get_or_insert(now);
        slot.record = Some(log.push(RestartRecord { at: now, worker: id, reason, attempt, downtime: None }));
        out.push(Action::Event(HealthEvent::Restarted { worker: id, reason, attempt }));
        let due = Seconds(now.0 + slot.backoff(attempt));
        slot.spawn_due = Some(due);
        slot.health = Health::Restarting { attempt };
        if !hung {
            Self::spawn_if_due(slot, id, now, out);
        }
    }
}
