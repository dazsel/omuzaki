//! The watchdog policy: when to restart, how often, and when to give up (spec v3 §25, BR-23;
//! `docs/design/supervisor.md` §5, §6, §15.4).
//!
//! A supervisor's decisions as a pure state machine: time and observations in, actions out. No processes, threads or
//! clock reads (`now` is a parameter), so every case is deterministic. [`Monitor::observe`] and [`Monitor::tick`]
//! reuse the caller's `Vec` and allocate nothing once it has capacity (a failed launch's text moves into the log, off
//! the hot path); [`HeartbeatCell::beat`] is one relaxed atomic add.
//!
//! The rules:
//! - **Starting** from `Spawned` until the first beat that advances the counter, or until `start_grace` passes without
//!   one (then `Hung { silent_for: start_grace }`). **Healthy** on that first advance.
//! - **Hung:** while Healthy, `now - last_advance >= hung_after`. **Crashed:** `Exited` while Starting, Restarting or
//!   Healthy. **Protocol:** `Faulted` (the runtime killed it for a malformed frame). **Launch failed:** the spawn or the
//!   handshake failed; it is an attempt like any, and `permanent` makes the worker `Failed` at once.
//! - On any of them: `Down` (once, until `Up`), `Kill` for a live hung instance, and `Event(Restarted)`. `Spawn` comes
//!   only after the instance has ended (never two instances): at once for attempt 1, after the backoff otherwise.
//! - **Backoff** for attempt `n` within the `within` window: 0 for `n = 1`, `min(backoff_first × 2^(n − 2),
//!   backoff_max)` after. A restart past `max_restarts` makes the worker `Failed` with one `GaveUp`, and nothing
//!   spawns until [`Monitor::request_restart`].
//! - **Stable:** `stable_after` of continuous Healthy resets the attempts and the window.
//! - **Recovered:** the first advancing beat after a respawn sends `Up` and reports the downtime since the first
//!   failure, `slow` past the worker's `slow_after`.
//! - **Stuck:** a kill that has not finished after the runtime's `exit_wait` changes nothing and emits one
//!   `Stuck`; the spawn still waits for `Exited`. **Abandoned** (threads and tasks only) counts as `Exited` for
//!   spawning and emits one `Abandoned`.
//! - **Ids** are `{ scope, instance, index }`: an id from another scope, another monitor or out of range is
//!   `Err(UnknownWorker)` from every method, never a panic.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_core::time::Seconds;
pub use omuzaki_stream_api::supervisor::{
    Health, HealthEvent, RestartReason, RestartRecord, Scope, UnknownWorker, WorkerId, WorkerKind, WorkerStatus,
};

/// The records the restart log keeps.
pub const RESTART_LOG_LEN: usize = 256;

/// Slack for comparing sums of seconds against deadlines.
const EPSILON: f64 = 1e-9;

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
    /// Its name, for people.
    pub name: String,
    /// What it does.
    pub kind: WorkerKind,
    /// How long a fresh instance may take to make progress for the first time.
    pub start_grace: Seconds,
    /// How long a healthy worker may stay silent.
    pub hung_after: Seconds,
    /// The downtime past which a recovery counts as slow.
    pub slow_after: Seconds,
    /// How it is restarted.
    pub restart: RestartPolicy,
}

impl WorkerSpec {
    /// The §5 and §6 defaults for `kind`. An encoder's `hung_after` assumes a recording at 30 fps with a queue of 16
    /// (`max(1 s, 4 × frame period × depth)` = 2.13 s); [`for_encoder`](Self::for_encoder) computes it for others.
    pub fn defaults_for(kind: WorkerKind, name: impl Into<String>) -> Self {
        let (start_grace, hung_after) = match kind {
            WorkerKind::Renderer => (5.0, 0.5),
            WorkerKind::Tracker => (10.0, 1.0),
            WorkerKind::Audio => (3.0, 0.25),
            WorkerKind::Encoder => (5.0, encoder_hung_after(30.0, 16)),
            _ => (30.0, 30.0),
        };
        let (backoff_first, backoff_max, max_restarts, within, stable_after, slow_after) = match kind {
            WorkerKind::Renderer | WorkerKind::Tracker | WorkerKind::Encoder => (0.25, 4.0, 5, 60.0, 30.0, 2.0),
            WorkerKind::Audio => (0.1, 2.0, 5, 60.0, 30.0, 0.5),
            _ => (1.0, 60.0, 10, 600.0, 300.0, 30.0),
        };
        Self {
            name: name.into(),
            kind,
            start_grace: Seconds(start_grace),
            hung_after: Seconds(hung_after),
            slow_after: Seconds(slow_after),
            restart: RestartPolicy {
                backoff_first: Seconds(backoff_first),
                backoff_max: Seconds(backoff_max),
                max_restarts,
                within: Seconds(within),
                stable_after: Seconds(stable_after),
            },
        }
    }

    /// An encoder's spec for a recording at `fps` with a queue of `depth` frames (record.md D11's hang rule).
    pub fn for_encoder(name: impl Into<String>, fps: f64, depth: u32) -> Self {
        let mut spec = Self::defaults_for(WorkerKind::Encoder, name);
        spec.hung_after = Seconds(encoder_hung_after(fps, depth));
        spec
    }
}

/// `max(1 s, 4 × frame period × depth)`.
fn encoder_hung_after(fps: f64, depth: u32) -> f64 {
    if fps <= 0.0 {
        return 1.0;
    }
    (4.0 / fps * f64::from(depth)).max(1.0)
}

/// A worker's progress and work counters: the worker (or the reader of its beats) advances them; the monitor reads
/// them. `beat` is wait-free.
#[derive(Clone, Debug, Default)]
pub struct HeartbeatCell(Arc<[AtomicU64; 2]>);

impl HeartbeatCell {
    /// Both counters at 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// One beat: a relaxed atomic add to the progress counter.
    pub fn beat(&self) {
        self.0[0].fetch_add(1, Ordering::Relaxed);
    }

    /// Sets the progress counter to a value the worker reported.
    pub fn set(&self, progress: u64) {
        self.0[0].store(progress, Ordering::Relaxed);
    }

    /// The progress counter.
    pub fn read(&self) -> u64 {
        self.0[0].load(Ordering::Relaxed)
    }

    /// Sets the work counter (frames, packets, blocks).
    pub fn set_work(&self, work: u64) {
        self.0[1].store(work, Ordering::Relaxed);
    }

    /// The work counter.
    pub fn work(&self) -> u64 {
        self.0[1].load(Ordering::Relaxed)
    }
}

/// What the supervisor saw happen to a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Observation {
    /// An instance started.
    Spawned,
    /// The instance's progress counter.
    Beat(u64),
    /// The instance ended.
    Exited {
        /// Its exit code; `None` when a signal killed it.
        code: Option<i32>,
    },
    /// The instance broke the protocol; the runtime killed it and it has ended.
    Faulted {
        /// What was wrong.
        what: String,
    },
    /// A kill has not finished after `exit_wait`: an event, no state change.
    Stuck,
    /// A hung thread or task given up (it can't be killed): treated as ended for spawning.
    Abandoned,
    /// The spawn or the handshake failed; any instance has ended.
    LaunchFailed {
        /// What failed.
        what: String,
        /// Retrying cannot help (a protocol version or generation mismatch, an executable that is not ours).
        permanent: bool,
    },
}

/// What the supervisor should do.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Action {
    /// Start an instance.
    Spawn(WorkerId),
    /// Kill the instance (never wait for it).
    Kill(WorkerId),
    /// The worker stopped serving: hold, hand off, mark a gap.
    Down(WorkerId),
    /// It serves again.
    Up(WorkerId),
    /// Tell the person.
    Event(HealthEvent),
}

/// The newest [`RESTART_LOG_LEN`] restarts.
#[derive(Clone, Debug)]
pub struct RestartLog {
    records: VecDeque<RestartRecord>,
    /// Records ever pushed; record `n` sits at `n - (pushed - len)` while it is kept.
    pushed: u64,
}

impl RestartLog {
    fn new() -> Self {
        Self { records: VecDeque::with_capacity(RESTART_LOG_LEN), pushed: 0 }
    }

    /// Appends `record`, dropping the oldest past the capacity; returns its sequence number.
    fn push(&mut self, record: RestartRecord) -> u64 {
        if self.records.len() == RESTART_LOG_LEN {
            self.records.pop_front();
        }
        self.records.push_back(record);
        self.pushed += 1;
        self.pushed - 1
    }

    fn get(&self, seq: u64) -> Option<&RestartRecord> {
        let first = self.pushed - self.records.len() as u64;
        self.records.get(usize::try_from(seq.checked_sub(first)?).ok()?)
    }

    fn get_mut(&mut self, seq: u64) -> Option<&mut RestartRecord> {
        let first = self.pushed - self.records.len() as u64;
        self.records.get_mut(usize::try_from(seq.checked_sub(first)?).ok()?)
    }

    /// The records, oldest first.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &RestartRecord> {
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
// Independent facts about the instance and the outputs; an enum of their combinations would be harder to read.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct Slot {
    spec: WorkerSpec,
    health: Health,
    /// An instance is running (spawned and not yet ended).
    alive: bool,
    /// A `Spawn` was issued and neither its `Spawned` nor its `LaunchFailed` has come.
    spawning: bool,
    /// The instance was killed; its end is expected, not a crash.
    killed: bool,
    /// `Stuck` was reported for the current kill.
    stuck: bool,
    /// When the next spawn is due: while backing off, or once the killed instance has ended.
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
    /// `Down` was sent and `Up` not yet.
    down: bool,
    /// The log record a recovery fills.
    record: Option<u64>,
    /// The newest log record of this worker.
    last: Option<u64>,
}

impl Slot {
    fn new(spec: WorkerSpec, now: Seconds) -> Self {
        Self {
            spec,
            health: Health::Starting,
            alive: false,
            spawning: false,
            killed: false,
            stuck: false,
            spawn_due: Some(now),
            started_at: now,
            last_beat: 0,
            last_advance: now,
            healthy_since: None,
            failed_at: None,
            attempts: 0,
            window_start: None,
            down: false,
            record: None,
            last: None,
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

    fn mark_down(&mut self, id: WorkerId, out: &mut Vec<Action>) {
        if !self.down {
            self.down = true;
            out.push(Action::Down(id));
        }
    }
}

/// The policy for a set of workers.
#[derive(Debug)]
pub struct Monitor {
    scope: Scope,
    instance: u64,
    slots: Vec<Slot>,
    log: RestartLog,
}

/// 64 random bits from the OS, never 0; the time and the process id if the OS refuses.
fn nonce() -> u64 {
    let mut bytes = [0_u8; 8];
    let value = if getrandom::fill(&mut bytes).is_ok() {
        u64::from_le_bytes(bytes)
    } else {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        u64::try_from(t & u128::from(u64::MAX)).unwrap_or(0) ^ (u64::from(std::process::id()) << 32)
    };
    value.max(1)
}

impl Monitor {
    /// Watches `specs` in `scope` (worker `i` is the `i`-th id of [`ids`](Self::ids)); each is due to spawn at `now`.
    /// The monitor's instance is a fresh random nonce, so its ids never name another monitor's workers.
    pub fn new(scope: Scope, specs: Vec<WorkerSpec>, now: Seconds) -> Self {
        Self::with_instance(scope, nonce(), specs, now)
    }

    /// As [`new`](Self::new), with a fixed instance (tests that compare ids across runs).
    pub fn with_instance(scope: Scope, instance: u64, specs: Vec<WorkerSpec>, now: Seconds) -> Self {
        Self {
            scope,
            instance,
            slots: specs.into_iter().map(|spec| Slot::new(spec, now)).collect(),
            log: RestartLog::new(),
        }
    }

    /// Every worker's id, in spec order: the only source of valid ids.
    pub fn ids(&self) -> impl Iterator<Item = WorkerId> + '_ {
        (0..self.slots.len()).map(|i| self.id(i))
    }

    fn id(&self, i: usize) -> WorkerId {
        WorkerId::new(self.scope, self.instance, u16::try_from(i).unwrap_or(u16::MAX))
    }

    /// The slot index of `id`.
    ///
    /// # Errors
    /// [`UnknownWorker`] for an id that is not this monitor's.
    pub fn index(&self, id: WorkerId) -> Result<usize, UnknownWorker> {
        let i = usize::from(id.index());
        if id.scope() != self.scope || id.instance() != self.instance || i >= self.slots.len() {
            return Err(UnknownWorker { id });
        }
        Ok(i)
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
    ///
    /// # Errors
    /// [`UnknownWorker`] for an id that is not this monitor's.
    pub fn spec(&self, id: WorkerId) -> Result<&WorkerSpec, UnknownWorker> {
        Ok(&self.slots[self.index(id)?].spec)
    }

    /// Takes in what happened to worker `id` at `now`; `out` is cleared first and gets the actions.
    ///
    /// # Errors
    /// [`UnknownWorker`] for an id that is not this monitor's (`out` is left empty).
    pub fn observe(
        &mut self,
        now: Seconds,
        id: WorkerId,
        obs: Observation,
        out: &mut Vec<Action>,
    ) -> Result<(), UnknownWorker> {
        out.clear();
        let i = self.index(id)?;
        let slot = &mut self.slots[i];
        match obs {
            Observation::Spawned => {
                if slot.alive {
                    return Ok(());
                }
                (slot.alive, slot.spawning, slot.killed, slot.stuck, slot.spawn_due) =
                    (true, false, false, false, None);
                (slot.started_at, slot.last_beat, slot.last_advance) = (now, 0, now);
            }
            Observation::Beat(count) => Self::beat(slot, &mut self.log, id, now, count, out),
            Observation::Exited { code } => {
                Self::ended(slot, &mut self.log, id, now, RestartReason::crashed(code), out);
            }
            Observation::Faulted { what } => {
                Self::ended(slot, &mut self.log, id, now, RestartReason::protocol(what), out);
            }
            Observation::Stuck => {
                if slot.alive && slot.killed && !slot.stuck {
                    slot.stuck = true;
                    out.push(Action::Event(HealthEvent::stuck(id)));
                }
            }
            Observation::Abandoned => {
                if slot.alive {
                    out.push(Action::Event(HealthEvent::abandoned(id)));
                    let reason = RestartReason::hung(Seconds((now.0 - slot.last_advance.0).max(0.0)));
                    Self::ended(slot, &mut self.log, id, now, reason, out);
                }
            }
            Observation::LaunchFailed { what, permanent } => {
                if !slot.alive && !slot.spawning {
                    return Ok(());
                }
                (slot.alive, slot.spawning, slot.killed, slot.stuck) = (false, false, false, false);
                if permanent {
                    Self::give_up(slot, &mut self.log, id, now, RestartReason::launch_failed(what), out);
                } else {
                    Self::fail(slot, &mut self.log, id, now, RestartReason::launch_failed(what), out);
                }
            }
        }
        Ok(())
    }

    fn beat(slot: &mut Slot, log: &mut RestartLog, id: WorkerId, now: Seconds, count: u64, out: &mut Vec<Action>) {
        if !slot.alive || slot.killed || count <= slot.last_beat {
            return;
        }
        (slot.last_beat, slot.last_advance) = (count, now);
        if matches!(slot.health, Health::Starting | Health::Restarting { .. }) {
            slot.health = Health::Healthy;
            slot.healthy_since = Some(now);
            if let Some(failed_at) = slot.failed_at.take() {
                if slot.down {
                    slot.down = false;
                    out.push(Action::Up(id));
                }
                let downtime = Seconds((now.0 - failed_at.0).max(0.0));
                let late = downtime.0 > slot.spec.slow_after.0;
                out.push(Action::Event(HealthEvent::recovered(id, downtime, late)));
                if let Some(record) = slot.record.take().and_then(|seq| log.get_mut(seq)) {
                    record.downtime = Some(downtime);
                }
            }
        }
    }

    /// The instance ended: an expected end after a kill spawns the next; otherwise it is a failure for `reason`.
    fn ended(
        slot: &mut Slot,
        log: &mut RestartLog,
        id: WorkerId,
        now: Seconds,
        reason: RestartReason,
        out: &mut Vec<Action>,
    ) {
        if !slot.alive {
            return;
        }
        slot.alive = false;
        slot.stuck = false;
        if slot.killed {
            slot.killed = false;
            Self::spawn_if_due(slot, id, now, out);
        } else if matches!(slot.health, Health::Starting | Health::Healthy | Health::Restarting { .. }) {
            Self::fail(slot, log, id, now, reason, out);
        }
    }

    /// Checks deadlines and due spawns at `now`; `out` is cleared first and gets the actions.
    pub fn tick(&mut self, now: Seconds, out: &mut Vec<Action>) {
        out.clear();
        for i in 0..self.slots.len() {
            let id = self.id(i);
            let slot = &mut self.slots[i];
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
                    let reason = RestartReason::hung(slot.spec.start_grace);
                    Self::fail(slot, &mut self.log, id, now, reason, out);
                }
                Health::Healthy if now.0 - slot.last_advance.0 + EPSILON >= slot.spec.hung_after.0 => {
                    let reason = RestartReason::hung(Seconds(now.0 - slot.last_advance.0));
                    Self::fail(slot, &mut self.log, id, now, reason, out);
                }
                Health::Healthy => slot.settle(now),
                _ => {}
            }
        }
    }

    /// A person's restart of worker `id` at `now`: clears the window, and revives a failed worker. `out` is cleared
    /// first and gets the actions.
    ///
    /// # Errors
    /// [`UnknownWorker`] for an id that is not this monitor's.
    pub fn request_restart(&mut self, now: Seconds, id: WorkerId, out: &mut Vec<Action>) -> Result<(), UnknownWorker> {
        out.clear();
        let i = self.index(id)?;
        let slot = &mut self.slots[i];
        (slot.attempts, slot.window_start) = (1, Some(now));
        slot.failed_at.get_or_insert(now);
        let seq = self.log.push(RestartRecord::new(now, id, RestartReason::Requested, 1));
        (slot.record, slot.last) = (Some(seq), Some(seq));
        slot.health = Health::restarting(1);
        slot.healthy_since = None;
        slot.spawn_due = Some(now);
        if !slot.alive {
            Self::spawn_if_due(slot, id, now, out);
        } else if !slot.killed {
            slot.mark_down(id, out);
            slot.killed = true;
            out.push(Action::Kill(id));
            out.push(Action::Event(HealthEvent::restarted(id, RestartReason::Requested, 1)));
        }
        Ok(())
    }

    /// Worker `id`'s health.
    ///
    /// # Errors
    /// [`UnknownWorker`] for an id that is not this monitor's.
    pub fn health(&self, id: WorkerId) -> Result<Health, UnknownWorker> {
        Ok(self.slots[self.index(id)?].health)
    }

    /// Worker `id`'s health-meter row, without a pid or a work count (the runtime knows those).
    ///
    /// # Errors
    /// [`UnknownWorker`] for an id that is not this monitor's.
    pub fn status(&self, id: WorkerId) -> Result<WorkerStatus, UnknownWorker> {
        let slot = &self.slots[self.index(id)?];
        let mut row = WorkerStatus::new(id, slot.spec.name.clone(), slot.spec.kind, slot.health);
        row.restarts_in_window = slot.attempts;
        row.last = slot.last.and_then(|seq| self.log.get(seq)).cloned();
        Ok(row)
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
            if let Health::Restarting { attempt, .. } = slot.health {
                slot.health = Health::backoff(attempt, due);
            }
            return;
        }
        slot.spawn_due = None;
        slot.spawning = true;
        if let Health::Backoff { attempt, .. } = slot.health {
            slot.health = Health::restarting(attempt);
        }
        out.push(Action::Spawn(id));
    }

    /// Out of chances at `now`: `Failed`, `Down`, one `GaveUp`.
    fn give_up(
        slot: &mut Slot,
        log: &mut RestartLog,
        id: WorkerId,
        now: Seconds,
        reason: RestartReason,
        out: &mut Vec<Action>,
    ) {
        slot.mark_down(id, out);
        let seq = log.push(RestartRecord::new(now, id, reason, slot.attempts + 1));
        slot.last = Some(seq);
        slot.health = Health::failed(slot.attempts);
        slot.spawn_due = None;
        slot.record = None;
        out.push(Action::Event(HealthEvent::gave_up(id, slot.attempts)));
    }

    /// The instance failed at `now`: count the restart, go down, kill a live one, and spawn or back off.
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
        let live = slot.alive;
        slot.mark_down(id, out);
        if live {
            slot.killed = true;
            out.push(Action::Kill(id));
        }
        let attempt = slot.attempts + 1;
        if attempt > slot.spec.restart.max_restarts {
            slot.health = Health::failed(slot.attempts);
            slot.spawn_due = None;
            slot.record = None;
            out.push(Action::Event(HealthEvent::gave_up(id, slot.attempts)));
            return;
        }
        slot.attempts = attempt;
        slot.window_start.get_or_insert(now);
        slot.failed_at.get_or_insert(now);
        let seq = log.push(RestartRecord::new(now, id, reason.clone(), attempt));
        (slot.record, slot.last) = (Some(seq), Some(seq));
        out.push(Action::Event(HealthEvent::restarted(id, reason, attempt)));
        let due = Seconds(now.0 + slot.backoff(attempt));
        slot.spawn_due = Some(due);
        slot.health = Health::restarting(attempt);
        if !live {
            Self::spawn_if_due(slot, id, now, out);
        }
    }
}
