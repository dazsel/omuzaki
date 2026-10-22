//! The [`EmissionLog`] (particles.md D7, D8, D12): the spawn batches the stage hands the renderer every frame, kept as
//! a fixed ring of bursts and stream segments. It is pure: no renderer, no clock (time is an argument), no hash map,
//! and no allocation after [`EmissionLog::new`]. Its output is a function of its calls and their times only, which is
//! what makes a take replay and a checkpoint restore exact.
//!
//! - **A burst** is one batch with `interval = 0`, thinned to what fits under the cap.
//! - **A stream** (weather, a fountain) is one open segment per [`StreamKey`]. Each frame its `count` is the births so
//!   far, `floor((t - start) / interval) + 1`. A change of what it spawns or of its rate closes the segment and opens
//!   the next at the next birth time, continuing the global index with the same seed, so the same flakes keep falling.
//! - **Prune** drops each batch's dead prefix (born more than `life_max` ago) by advancing `first` and `start`, and
//!   removes closed batches left empty; a two-hour stream stays about `rate × life_max` slots.
//! - **Clear** shrinks every batch's `scale` to 0 over `fade_s`, then drops them.
//! - **The cap.** The live sum of `count` (slots) stays at or under `max_slots`; weather takes at most
//!   `weather_share` of it, so reactions always have room.

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::limits::{MAX_PARTICLES, MAX_SPAWN_BATCHES};
use omuzaki_render_api::{CellPick, ColourSet, EmitterShape, ParticleSystemId, SpawnBatch};

/// Open streams at once.
pub const MAX_STREAMS: usize = 16;

/// The cap on live slots and the share weather may take (D8).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EffectLimits {
    /// The most slots alive at once.
    pub max_slots: u32,
    /// The share of `max_slots` weather streams may hold, `0..=1`.
    pub weather_share: f32,
}

impl EffectLimits {
    /// While performing: 5,000 slots, weather at most 70 % of them.
    pub const fn live() -> Self {
        Self { max_slots: 5_000, weather_share: 0.7 }
    }

    /// For renders: the render contract's limit, weather at most 70 %.
    pub const fn offline() -> Self {
        Self { max_slots: MAX_PARTICLES, weather_share: 0.7 }
    }

    /// The slots weather may hold.
    pub fn weather_slots(&self) -> u32 {
        // In range by construction: the share is 0..=1 of a u32.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let slots = (f64::from(self.max_slots) * f64::from(self.weather_share.clamp(0.0, 1.0))).floor() as u32;
        slots
    }
}

/// What a batch spawns, without its timing: a [`SpawnBatch`] less `start`, `interval`, `first` and `count`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Spawn {
    /// The system.
    pub system: ParticleSystemId,
    /// Emitter to world.
    pub emitter: Mat4,
    /// Where in the emitter.
    pub shape: EmitterShape,
    /// The anchor's motion, metres per second.
    pub velocity: Vec3,
    /// The seed.
    pub seed: u32,
    /// Atlas cells.
    pub cells: CellPick,
    /// A colour override.
    pub colours: Option<ColourSet>,
    /// A size multiplier.
    pub scale: f32,
}

impl Spawn {
    /// `system` at `emitter`'s origin with seed 0 and nothing else set.
    pub const fn new(system: ParticleSystemId, emitter: Mat4) -> Self {
        Self {
            system,
            emitter,
            shape: EmitterShape::Point,
            velocity: Vec3::ZERO,
            seed: 0,
            cells: CellPick::All,
            colours: None,
            scale: 1.0,
        }
    }

    fn batch(&self, start: Seconds, interval: f32, first: u32, count: u32) -> SpawnBatch {
        SpawnBatch {
            shape: self.shape,
            velocity: self.velocity,
            start,
            interval,
            first,
            count,
            seed: self.seed,
            cells: self.cells,
            colours: self.colours,
            scale: self.scale,
            ..SpawnBatch::new(self.system, self.emitter)
        }
    }

    /// The spawn a batch was made from.
    pub fn of(batch: &SpawnBatch) -> Self {
        Self {
            system: batch.system,
            emitter: batch.emitter,
            shape: batch.shape,
            velocity: batch.velocity,
            seed: batch.seed,
            cells: batch.cells,
            colours: batch.colours,
            scale: batch.scale,
        }
    }
}

/// A stream's name: weather, a fountain. Each key has at most one open segment.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamKey(pub u32);

/// What a burst came to.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BurstOutcome {
    /// Particles spawned.
    pub count: u32,
    /// Particles the cap (or a full ring) took away.
    pub thinned: u32,
}

/// What the log knows about a batch beside the batch itself.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Meta {
    /// The longest lifetime of its system: older particles are dead.
    pub life_max: f32,
    /// The stream slot whose open segment this is.
    pub open: Option<u8>,
    /// Counts against the weather share.
    pub weather: bool,
    /// Its scale before a clear started shrinking it, while one does.
    pub fading: Option<f32>,
}

/// A stream's state between frames.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StreamState {
    /// Its key.
    pub key: StreamKey,
    /// What it spawns.
    pub spawn: Spawn,
    /// Births per second asked for.
    pub rate: f32,
    /// Births per second after the weather share.
    pub effective: f32,
    /// The longest lifetime.
    pub life_max: f32,
    /// Counts against the weather share.
    pub weather: bool,
    /// The global index the next segment starts at.
    pub next_first: u32,
    /// When the next segment's first particle is born, if a segment closed (a stream that continues).
    pub next_birth: Option<Seconds>,
}

/// An ongoing clear.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Clear {
    from: Seconds,
    fade_s: f32,
}

/// The stage's spawn batches: bursts and stream segments in birth order (D12).
#[derive(Clone, Debug, PartialEq)]
pub struct EmissionLog {
    limits: EffectLimits,
    batches: Vec<SpawnBatch>,
    meta: Vec<Meta>,
    streams: [Option<StreamState>; MAX_STREAMS],
    clear: Option<Clear>,
    thinned: u64,
}

impl EmissionLog {
    /// An empty log under `limits`, its ring allocated once.
    pub fn new(limits: EffectLimits) -> Self {
        Self {
            limits,
            batches: Vec::with_capacity(MAX_SPAWN_BATCHES),
            meta: Vec::with_capacity(MAX_SPAWN_BATCHES),
            streams: [None; MAX_STREAMS],
            clear: None,
            thinned: 0,
        }
    }

    /// The limits.
    pub fn limits(&self) -> EffectLimits {
        self.limits
    }

    /// Changes the limits (live while performing, offline for a render). Takes effect for later births.
    pub fn set_limits(&mut self, limits: EffectLimits) {
        self.limits = limits;
    }

    /// The live sum of `count`: an upper bound on live particles.
    pub fn slots(&self) -> u32 {
        self.batches.iter().map(|b| b.count).sum()
    }

    fn weather_slots(&self) -> u32 {
        self.batches.iter().zip(&self.meta).filter(|(_, m)| m.weather).map(|(b, _)| b.count).sum()
    }

    /// Particles the cap has thinned away since the log was made.
    pub fn thinned(&self) -> u64 {
        self.thinned
    }

    /// The batches, in birth order.
    pub fn batches(&self) -> &[SpawnBatch] {
        &self.batches
    }

    /// What the log knows about each batch, in the order of [`batches`](Self::batches).
    pub fn meta(&self) -> &[Meta] {
        &self.meta
    }

    /// The streams, by slot.
    pub fn streams(&self) -> &[Option<StreamState>; MAX_STREAMS] {
        &self.streams
    }

    /// Whether any batch draws from `system`.
    pub fn uses(&self, system: ParticleSystemId) -> bool {
        self.batches.iter().any(|b| b.system == system)
            || self.streams.iter().flatten().any(|s| s.spawn.system == system && s.effective > 0.0)
    }

    /// Whether nothing is alive and no stream is open.
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty() && self.streams.iter().flatten().all(|s| s.effective <= 0.0)
    }

    fn push(&mut self, batch: SpawnBatch, meta: Meta) -> bool {
        if self.batches.len() >= MAX_SPAWN_BATCHES {
            return false;
        }
        self.batches.push(batch);
        self.meta.push(meta);
        true
    }

    /// Spawns `count` particles of `spawn` at `t`, thinned to the room under the cap; nothing when the ring is full.
    pub fn burst(&mut self, t: Seconds, spawn: &Spawn, count: u32, life_max: f32) -> BurstOutcome {
        let room = self.limits.max_slots.saturating_sub(self.slots());
        let fits = count.min(room);
        let meta = Meta { life_max, open: None, weather: false, fading: None };
        let spawned = if fits > 0 && self.push(spawn.batch(t, 0.0, 0, fits), meta) { fits } else { 0 };
        let outcome = BurstOutcome { count: spawned, thinned: count - spawned };
        self.thinned += u64::from(outcome.thinned);
        outcome
    }

    /// Keeps the stream `key` going at `t`: `rate` births a second of `spawn`, each living at most `life_max`. A
    /// changed `spawn` or `rate` closes the open segment and opens the next at the next birth time; `rate = 0` closes
    /// it. A weather stream's rate is lowered to keep its slots within the weather share. Returns false when no
    /// stream slot is free.
    pub fn stream(
        &mut self,
        key: StreamKey,
        t: Seconds,
        spawn: &Spawn,
        rate: f32,
        life_max: f32,
        weather: bool,
    ) -> bool {
        self.stream_from(key, t, spawn, rate, life_max, weather, None)
    }

    /// Opens the stream `key` with its first segment started `life_max` ago, so the first frame shows a stream in
    /// full flow and the same every time (D7). An open segment of `key` is closed first.
    pub fn prewarm(
        &mut self,
        key: StreamKey,
        t: Seconds,
        spawn: &Spawn,
        rate: f32,
        life_max: f32,
        weather: bool,
    ) -> bool {
        self.stream_from(key, t, spawn, rate, life_max, weather, Some(Seconds(t.0 - f64::from(life_max))))
    }

    #[allow(clippy::too_many_arguments)] // the public pair's arguments plus the pre-warm start
    fn stream_from(
        &mut self,
        key: StreamKey,
        t: Seconds,
        spawn: &Spawn,
        rate: f32,
        life_max: f32,
        weather: bool,
        start: Option<Seconds>,
    ) -> bool {
        let rate = if rate.is_finite() { rate.max(0.0) } else { 0.0 };
        let effective = if weather {
            // Slots alive are births in one lifetime plus the one being born: hold them to the share.
            let share = f64::from(self.limits.weather_slots().saturating_sub(1)) / f64::from(life_max.max(1e-3));
            #[allow(clippy::cast_possible_truncation)]
            let share = share as f32;
            rate.min(share)
        } else {
            rate
        };
        let found = self.streams.iter().position(|s| s.is_some_and(|s| s.key == key));
        let Some(slot) = found.or_else(|| self.streams.iter().position(Option::is_none)) else { return false };
        // Exact equality is the point: any change of value opens a new segment.
        #[allow(clippy::float_cmp)]
        let same = self.streams[slot].is_some_and(|s| {
            s.spawn == *spawn && s.effective == effective && s.weather == weather && s.life_max == life_max
        });
        // The open segment first takes every birth up to now at its old values.
        self.extend(slot, t);
        let open = self.open_segment(slot);
        if !same || start.is_some() || open.is_none() && effective > 0.0 {
            // Close whatever is open: its batch keeps the births it has, and the stream remembers where it stopped.
            let (next_first, next_birth) = match (self.streams[slot], open) {
                (Some(_), Some(i)) => {
                    let b = self.batches[i];
                    self.meta[i].open = None;
                    (b.first + b.count, Some(b.birth(b.first + b.count)))
                }
                (Some(state), None) => (state.next_first, state.next_birth),
                (None, _) => (0, None),
            };
            let state = StreamState { key, spawn: *spawn, rate, effective, life_max, weather, next_first, next_birth };
            self.streams[slot] = Some(state);
            if effective > 0.0 {
                let interval = 1.0 / effective;
                let begin = start.unwrap_or_else(|| next_birth.filter(|b| b.0 >= t.0).unwrap_or(t));
                let meta = Meta { life_max, open: u8::try_from(slot).ok(), weather, fading: None };
                if !self.push(spawn.batch(begin, interval, next_first, 0), meta) {
                    return true;
                }
            }
        }
        self.extend(slot, t);
        true
    }

    /// The batch index of stream `slot`'s open segment.
    fn open_segment(&self, slot: usize) -> Option<usize> {
        let slot = u8::try_from(slot).ok()?;
        self.meta.iter().position(|m| m.open == Some(slot))
    }

    /// Brings stream `slot`'s open segment up to the births at `t`, within the cap and the weather share.
    fn extend(&mut self, slot: usize, t: Seconds) {
        let Some(i) = self.open_segment(slot) else { return };
        let b = self.batches[i];
        let born = if t.0 < b.start.0 || b.interval <= 0.0 {
            0
        } else {
            // In range: births so far of a u32-indexed stream.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = (((t.0 - b.start.0) / f64::from(b.interval)).floor() + 1.0).min(f64::from(u32::MAX)) as u32;
            n
        };
        let room = self.limits.max_slots.saturating_sub(self.slots() - b.count);
        let mut want = born.min(room).min(u32::MAX - b.first);
        if self.meta[i].weather {
            let weather_room = self.limits.weather_slots().saturating_sub(self.weather_slots() - b.count);
            want = want.min(weather_room);
        }
        // Never shrink: a particle once born stays until prune drops it.
        self.batches[i].count = want.max(b.count.min(born));
    }

    /// Advances every open stream to `t` without changing what it spawns: the per-frame step.
    pub fn advance(&mut self, t: Seconds) {
        for slot in 0..MAX_STREAMS {
            self.extend(slot, t);
        }
    }

    /// Drops what is dead at `t`: each batch's particles born more than `life_max` before `t` (advancing `first` and
    /// `start`), closed batches left empty, bursts older than `life_max`, and the batches a finished clear shrank.
    pub fn prune(&mut self, t: Seconds) {
        self.advance(t);
        let clear = self.clear;
        let shrink = clear.map(|c| {
            #[allow(clippy::cast_possible_truncation)] // a fraction of a fade, clamped below
            let k = if c.fade_s > 0.0 { ((t.0 - c.from.0) / f64::from(c.fade_s)) as f32 } else { 1.0 };
            k.clamp(0.0, 1.0)
        });
        let mut keep = 0;
        for i in 0..self.batches.len() {
            let mut b = self.batches[i];
            let m = self.meta[i];
            let dead = if b.interval > 0.0 {
                let cutoff = t.0 - f64::from(m.life_max);
                if cutoff < b.start.0 {
                    0
                } else {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let n =
                        (((cutoff - b.start.0) / f64::from(b.interval)).floor() + 1.0).min(f64::from(b.count)) as u32;
                    n
                }
            } else if t.0 - b.start.0 > f64::from(m.life_max) {
                b.count
            } else {
                0
            };
            if dead > 0 {
                b.start = b.birth(b.first + dead);
                b.first += dead;
                b.count -= dead;
            }
            let mut gone = b.count == 0 && m.open.is_none();
            if let (Some(base), Some(k)) = (m.fading, shrink) {
                if k >= 1.0 {
                    gone = true;
                } else {
                    b.scale = (base * (1.0 - k)).max(f32::MIN_POSITIVE);
                }
            }
            if gone {
                if let Some(slot) = m.open {
                    self.close_stream(usize::from(slot), &b);
                }
                continue;
            }
            self.batches[keep] = b;
            self.meta[keep] = m;
            keep += 1;
        }
        self.batches.truncate(keep);
        self.meta.truncate(keep);
        if shrink.is_some_and(|k| k >= 1.0) {
            self.clear = None;
        }
    }

    /// Records where a stream stopped when its open batch goes away.
    fn close_stream(&mut self, slot: usize, b: &SpawnBatch) {
        if let Some(state) = self.streams.get_mut(slot).and_then(Option::as_mut) {
            state.next_first = b.first + b.count;
            state.next_birth = Some(b.birth(b.first + b.count));
        }
    }

    /// Closes every stream and shrinks every batch to nothing between `t` and `t + fade_s`, after which they are
    /// dropped. Later births are not affected.
    pub fn clear(&mut self, t: Seconds, fade_s: f32) {
        for slot in 0..MAX_STREAMS {
            if let Some(i) = self.open_segment(slot) {
                self.meta[i].open = None;
                let b = self.batches[i];
                self.close_stream(slot, &b);
            }
            if let Some(state) = self.streams[slot].as_mut() {
                state.rate = 0.0;
                state.effective = 0.0;
            }
        }
        for (b, m) in self.batches.iter().zip(self.meta.iter_mut()) {
            m.fading = Some(m.fading.unwrap_or(b.scale));
        }
        self.clear = Some(Clear { from: t, fade_s: fade_s.max(0.0) });
        if fade_s <= 0.0 {
            self.prune(t);
        }
    }

    /// Stops the stream `key` and forgets it: its batches finish their lives. Its slot is free again.
    pub fn forget_stream(&mut self, key: StreamKey) {
        if let Some(slot) = self.streams.iter().position(|s| s.is_some_and(|s| s.key == key)) {
            if let Some(i) = self.open_segment(slot) {
                self.meta[i].open = None;
            }
            self.streams[slot] = None;
        }
    }

    /// Copies the live batches into `out` in birth order (what `Scene::particles` gets).
    pub fn fill(&self, out: &mut Vec<SpawnBatch>) {
        out.extend_from_slice(&self.batches);
    }

    /// Renames every system: what a checkpoint restore into another renderer does. `map` gives the new id of each.
    pub fn remap(&mut self, map: &dyn Fn(ParticleSystemId) -> ParticleSystemId) {
        for b in &mut self.batches {
            b.system = map(b.system);
        }
        for state in self.streams.iter_mut().flatten() {
            state.spawn.system = map(state.spawn.system);
        }
    }
}
