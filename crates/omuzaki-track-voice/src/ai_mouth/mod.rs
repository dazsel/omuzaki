//! The AI performer's mouth drive (`ai-performer-voice-face.md` §4.1 to §4.3, AP-05): the TTS host's PCM, heard
//! [`lead`] ahead of the viewers, through a `voice_to_face@1` stream into a `Source::Audio` layer, stamped with the
//! program time its audio actually played.
//!
//! ```text
//!  TTS pump ──VoiceLead::push(at, feed index, pcm)──▶ MouthDrive ──BlockSink::push / BlockSource::take──▶ keys
//!                                                        │ ticks every 10 ms of voice, evaluated from the keys
//!  play log + cut flag (CutProbe) ─────────────────────▶ │ released once their audio played, stamped P − D
//!                                                        ▼
//!                                         (stamp, Layer) to the stage's PlayoutDelay
//! ```
//!
//! **One timeline** (§4.2). `v` is seconds of voice since the generation's first sample, on the lead PCM (no gaps).
//! - *Keys.* Block `k` of the model (rate `f`, block `B`, latency `L`) is the key at `v = (kB − L)/f`; the first
//!   `W = ⌈L/B⌉` blocks are pre-roll and dropped; when `WB > L` a **head key** at `v = 0` is the viseme frame of
//!   `[0, WB − L)`. A missed block's key is the viseme frame of the same interval; a ready one is
//!   `w·model + (1 − w)·viseme`, `w` moving by `(B/f)/50 ms` per block toward 1 on ready and 0 on missed, pinned at 0
//!   while the stream is stalled.
//! - *Ticks.* Every 10 ms of voice, the linear interpolation of the keys around it, evaluated when it reaches the head
//!   of the queue. A key not decided by then is replaced by its viseme key and counted (`late_keys`): a slow model
//!   costs fidelity, never timing.
//! - *Release.* Only the play log says a tick's audio played: the tick at `v` goes when a record holds its feed sample,
//!   stamped from **that** record (`P(v) − D`), so stamps rise by construction and never pass a cut. Before each
//!   release the probe is checked: `Raised` releases nothing; `Cut { at }` releases what played by `at`, drops the
//!   rest and closes the mouth over 60 ms from `max(at, newest stamp)`.
//! - *Stalls.* When the play log stops (an underrun) for a block, the mouth fades to rest over 60 ms from where the log
//!   stopped, item by item as each falls due; overdue items collapse into the newest; when the audio resumes the rest
//!   are dropped and a resume tick lands where the audio did.
//! - *Generations.* A cut restarts the stream (the next reply never inherits the cut word's model state), and so does
//!   PCM arriving after a tail was flushed; each keeps its own sequence origin.
//! - *Private* PCM (§4.3) leaves the layer clear.
//!
//! Hot path: [`MouthDrive::push`] and [`MouthDrive::poll`] allocate nothing once the drive is built (`tests/no_alloc.rs`);
//! [`thread`] runs it.

// Sample counts and indices stay far below 2^52 (and 2^63): the casts between them and `f64` or `i64` are exact.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

pub mod thread;
pub mod visemes;

use omuzaki_ai_api::{Block, BlockSink, BlockSource, StreamSpec, TensorBuf, TensorRef};
use omuzaki_anim_api::{ARKIT_COUNT, BlendMode, Channel, ChannelBuf, ChannelMask, Layer};
use omuzaki_core::time::Seconds;
use omuzaki_performer_api::voice::{CutProbe, CutState, PlayGap};

pub use thread::{MouthHandle, MouthOut, MouthThread};
pub use visemes::{EnergyVisemes, JAW_PER_RMS, Visemes};

/// A tick: 10 ms of voice.
pub const TICK_S: f64 = 0.010;
/// A fade (stall or close) is this many ticks to rest.
pub const FADE_STEPS: u32 = 6;
/// The crossfade between the model and the visemes, seconds.
pub const CROSSFADE_S: f64 = 0.050;
/// The lead never exceeds this (§4.1): beyond it, undecided keys become visemes instead of delaying the voice.
pub const LEAD_CAP_S: f64 = 0.100;
/// The pump's own granularity, in the lead: it writes every 10 ms.
pub const PUMP_S: f64 = 0.010;

/// The model-rate PCM kept per generation, samples: 4 s at 16 kHz, far more than any key looks back.
const PCM_RING: usize = 1 << 16;
/// Keys kept, blocks: 5 s of 20 ms blocks.
const KEY_RING: usize = 256;
/// Contiguous runs of lead PCM kept per generation (chunks and the pads between them).
const SEGMENTS: usize = 128;
/// The step that keeps stamps strictly rising when two would tie.
const EPSILON_S: f64 = 1e-6;
/// The engine block assumed until the play log shows one: 1,024 frames at 48 kHz.
const DEFAULT_ENGINE_BLOCK_S: f64 = 1024.0 / 48_000.0;

/// `lead_min = (queue + 2)·B/f + L/f + 10 ms` (§4.2): the lead at which every tick's keys are decided before it is due.
pub fn lead_min(spec: &StreamSpec, queue: u32) -> Seconds {
    let f = f64::from(spec.sample_rate.max(1));
    Seconds((f64::from(queue + 2) * f64::from(spec.block) + f64::from(spec.latency)) / f + PUMP_S)
}

/// The lead the drive asks of the pump: [`lead_min`], at most [`LEAD_CAP_S`].
pub fn lead(spec: &StreamSpec, queue: u32) -> Seconds {
    Seconds(lead_min(spec, queue).0.min(LEAD_CAP_S))
}

/// How the drive behaves.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MouthConfig {
    /// The A/V delay `D`: every stamp is the program time less this (anim-api §6.1).
    pub delay: Seconds,
    /// The lead blocks are pushed with; `None` for [`lead`].
    pub lead: Option<Seconds>,
    /// The channels the layer drives: the mouth, `jawOpen` and `tongueOut` included.
    pub mask: ChannelMask,
}

impl Default for MouthConfig {
    fn default() -> Self {
        Self { delay: Seconds(0.0), lead: None, mask: ChannelMask::MOUTH }
    }
}

/// Counters for the health meter (`Copy`: reading them never allocates).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MouthStats {
    /// Ticks released.
    pub ticks: u64,
    /// Ticks evaluated with a key the model had not decided: "mouth: visemes N %" is `late_keys / ticks`.
    pub late_keys: u64,
    /// Keys the model missed (the viseme took their place).
    pub missed_keys: u64,
    /// Fade items that fell due together and collapsed into the newest.
    pub fade_overdue: u64,
    /// Ticks whose play-log record was reused before it was read (`mouth_history_lost`).
    pub history_lost: u64,
    /// Ticks dropped because their audio was cut.
    pub cut_ticks: u64,
    /// Items not released because their stamp did not rise (always 0; a test asserts it).
    pub refused: u64,
    /// Generations started.
    pub generations: u64,
}

/// A run of lead PCM that is contiguous on the feed.
#[derive(Copy, Clone, Debug, Default)]
struct Segment {
    /// Voice time of its first sample.
    v: f64,
    /// Samples.
    len: u64,
    /// The lead's rate.
    rate: u32,
    /// Feed index of its first sample.
    feed: u64,
    /// Predicted program time of its first sample (pacing only).
    at: f64,
}

impl Segment {
    fn end(&self) -> f64 {
        self.v + self.len as f64 / f64::from(self.rate)
    }
}

/// A decided key.
#[derive(Copy, Clone, Debug)]
struct Key {
    /// Block number in the generation; `u64::MAX` for an empty slot.
    k: u64,
    values: ChannelBuf,
}

const NO_KEY: Key = Key { k: u64::MAX, values: ChannelBuf::NEUTRAL };

/// One generation: one reply, one stream origin, one play log.
#[allow(clippy::struct_excessive_bools)] // four independent facts about a reply
struct Generation {
    live: bool,
    probe: Option<CutProbe>,
    /// The probe's cut was handled (or the generation ended): no more ticks.
    closed: bool,
    /// Takes no more PCM: a later push starts the next generation.
    sealed: bool,
    segments: Box<[Segment]>,
    segment_count: usize,
    /// Seconds of voice received.
    voice: f64,
    // Resampling the lead to the model rate: output `m` is at lead position `m·step` of the current rate.
    step: f64,
    next_pos: f64,
    last_in: f32,
    inputs: u64,
    pcm: Box<[f32]>,
    model_len: u64,
    seq0: Option<u64>,
    pushed: u64,
    taken: u64,
    /// Silent blocks still to push in the tail; `None` before the tail.
    tail: Option<u32>,
    keys: Box<[Key]>,
    w: f32,
    last_model: ChannelBuf,
    next_tick: u64,
    /// The play-log record that held the newest released tick.
    record_cursor: u64,
    /// The program time the play log stopped at, while a stall is in progress.
    stall: Option<f64>,
    /// The end-of-voice close was started.
    ended: bool,
}

impl Generation {
    fn new() -> Self {
        Self {
            live: false,
            probe: None,
            closed: false,
            sealed: false,
            segments: vec![Segment::default(); SEGMENTS].into(),
            segment_count: 0,
            voice: 0.0,
            step: 1.0,
            next_pos: 0.0,
            last_in: 0.0,
            inputs: 0,
            pcm: vec![0.0; PCM_RING].into(),
            model_len: 0,
            seq0: None,
            pushed: 0,
            taken: 0,
            tail: None,
            keys: vec![NO_KEY; KEY_RING].into(),
            w: 1.0,
            last_model: ChannelBuf::NEUTRAL,
            next_tick: 0,
            record_cursor: 0,
            stall: None,
            ended: false,
        }
    }

    /// Starts over on `probe`, keeping the allocations.
    fn reset(&mut self, probe: Option<CutProbe>) {
        let (segments, pcm, keys) =
            (std::mem::take(&mut self.segments), std::mem::take(&mut self.pcm), std::mem::take(&mut self.keys));
        *self = Self { segments, pcm, keys, ..Self::empty() };
        self.keys.fill(NO_KEY);
        self.live = true;
        self.probe = probe;
    }

    /// A generation with no buffers, only for [`reset`](Self::reset) to fill in.
    fn empty() -> Self {
        Self {
            live: false,
            probe: None,
            closed: false,
            sealed: false,
            segments: Box::default(),
            segment_count: 0,
            voice: 0.0,
            step: 1.0,
            next_pos: 0.0,
            last_in: 0.0,
            inputs: 0,
            pcm: Box::default(),
            model_len: 0,
            seq0: None,
            pushed: 0,
            taken: 0,
            tail: None,
            keys: Box::default(),
            w: 1.0,
            last_model: ChannelBuf::NEUTRAL,
            next_tick: 0,
            record_cursor: 0,
            stall: None,
            ended: false,
        }
    }

    fn segment(&self, i: usize) -> &Segment {
        &self.segments[i % SEGMENTS]
    }

    /// The segments still held, oldest first.
    fn held(&self) -> std::ops::Range<usize> {
        self.segment_count.saturating_sub(SEGMENTS)..self.segment_count
    }

    /// The feed index of voice time `v`, `None` when no segment holds it.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // offsets inside a segment
    fn feed_of(&self, v: f64) -> Option<u64> {
        self.held().rev().map(|i| self.segment(i)).find(|s| s.v <= v + 1e-12 && v < s.end()).map(|s| {
            let offset = ((v - s.v) * f64::from(s.rate) + 1e-6).floor().max(0.0) as u64;
            s.feed + offset.min(s.len - 1)
        })
    }

    /// The voice time of feed sample `sample`, `None` when it isn't this generation's.
    #[allow(clippy::cast_precision_loss)]
    fn voice_of(&self, sample: u64) -> Option<f64> {
        self.held()
            .rev()
            .map(|i| self.segment(i))
            .find(|s| (s.feed..s.feed + s.len).contains(&sample))
            .map(|s| s.v + (sample - s.feed) as f64 / f64::from(s.rate))
    }

    /// The predicted program time of voice time `v` (pacing only), extrapolated past the ends.
    fn at_of(&self, v: f64) -> f64 {
        let held = self.held();
        if held.is_empty() {
            return f64::INFINITY;
        }
        let found = held.clone().rev().map(|i| self.segment(i)).find(|s| s.v <= v);
        let s = found.unwrap_or_else(|| self.segment(held.start));
        s.at + (v - s.v)
    }

    /// Appends lead PCM, resampled to `f`.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn append(&mut self, at: f64, feed: u64, rate: u32, pcm: &[f32], f: u32) {
        let rate = rate.max(1);
        let n = pcm.len() as u64;
        let last = (self.segment_count > 0).then(|| *self.segment(self.segment_count - 1));
        match last {
            Some(s) if s.rate == rate && s.feed + s.len == feed => {
                self.segments[(self.segment_count - 1) % SEGMENTS].len += n;
            }
            _ => {
                self.segments[self.segment_count % SEGMENTS] = Segment { v: self.voice, len: n, rate, feed, at };
                self.segment_count += 1;
            }
        }
        self.voice += pcm.len() as f64 / f64::from(rate);
        // Linear interpolation; a rate change takes effect from this push's first sample.
        let step = f64::from(rate) / f64::from(f);
        if (step - self.step).abs() > f64::EPSILON {
            // Keep the next output's place in time: re-express it in the new rate's sample units.
            let into = self.next_pos - self.inputs as f64;
            self.next_pos = self.inputs as f64 + into * self.step / step;
            self.step = step;
        }
        for &x in pcm {
            let i = self.inputs as f64;
            while self.next_pos <= i {
                let frac = (self.next_pos - (i - 1.0)) as f32;
                let y = if self.inputs == 0 { x } else { self.last_in + (x - self.last_in) * frac.clamp(0.0, 1.0) };
                self.pcm[(self.model_len % PCM_RING as u64) as usize] = y;
                self.model_len += 1;
                self.next_pos += self.step;
            }
            self.last_in = x;
            self.inputs += 1;
        }
    }

    /// Model-rate samples `[from, to)` into `out` (zeros outside what is held), returning the slice.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn copy<'a>(&self, from: i64, to: i64, out: &'a mut [f32]) -> &'a [f32] {
        let len = (to - from).clamp(0, out.len() as i64) as usize;
        let oldest = self.model_len.saturating_sub(PCM_RING as u64) as i64;
        for (j, o) in out[..len].iter_mut().enumerate() {
            let u = from + j as i64;
            *o = if u >= oldest && u < self.model_len as i64 {
                self.pcm[(u as u64 % PCM_RING as u64) as usize]
            } else {
                0.0
            };
        }
        &out[..len]
    }

    fn key(&self, k: u64) -> Option<&ChannelBuf> {
        let slot = &self.keys[(k % KEY_RING as u64) as usize];
        (slot.k == k).then_some(&slot.values)
    }

    /// Ticks waiting for their audio.
    #[allow(clippy::cast_precision_loss)]
    fn pending(&self) -> bool {
        self.live && !self.closed && (self.next_tick as f64) * TICK_S < self.voice - 1e-9
    }
}

/// An active fade to rest: a stall's or a close's six items `start + 10 ms·k`.
#[derive(Copy, Clone, Debug)]
struct Fade {
    close: bool,
    start: f64,
    from: ChannelBuf,
    next: u32,
}

/// The drive. Push from the pump with [`push`](Self::push), [`attach`](Self::attach) and [`cut`](Self::cut); call
/// [`poll`](Self::poll) from the release thread at least twice per engine block.
pub struct MouthDrive {
    sink: Box<dyn BlockSink>,
    source: Box<dyn BlockSource>,
    spec: StreamSpec,
    queue: u64,
    /// Pre-roll blocks: `⌈L/B⌉`.
    warm: u64,
    lead: f64,
    config: MouthConfig,
    visemes: Box<dyn Visemes>,
    outputs: Vec<TensorBuf>,
    block: Box<[f32]>,
    scratch: Box<[f32]>,
    shape: [usize; 1],
    gens: [Generation; 2],
    /// Index of the current generation; the other is retiring (its ticks still releasing).
    current: usize,
    /// Program time and values of the newest released item.
    last_p: f64,
    last_values: ChannelBuf,
    last_clear: bool,
    fade: Option<Fade>,
    /// The program time private speech starts, and whether the clear was released.
    private_from: Option<f64>,
    cleared: bool,
    /// The longest span of one play-log record seen: one engine block.
    engine_block: f64,
    stats: MouthStats,
}

impl MouthDrive {
    /// A drive over one `voice_to_face@1` stream (`spec` its model's, `outputs` sized for its signature's outputs:
    /// `TensorBuf::for_outputs`), with `visemes` as the fallback. Cold: allocates.
    pub fn new(
        sink: Box<dyn BlockSink>,
        source: Box<dyn BlockSource>,
        spec: StreamSpec,
        outputs: Vec<TensorBuf>,
        visemes: Box<dyn Visemes>,
        config: MouthConfig,
    ) -> Self {
        let queue = source.queue();
        let block = spec.block.max(1) as usize;
        let lead = config.lead.unwrap_or_else(|| lead(&spec, queue)).0;
        Self {
            sink,
            source,
            spec,
            queue: u64::from(queue),
            warm: u64::from(spec.latency.div_ceil(spec.block.max(1))),
            lead,
            config,
            visemes,
            outputs,
            block: vec![0.0; block].into(),
            scratch: vec![0.0; block * 2].into(),
            shape: [block],
            gens: [Generation::new(), Generation::new()],
            current: 0,
            last_p: f64::NEG_INFINITY,
            last_values: ChannelBuf::NEUTRAL,
            last_clear: true,
            fade: None,
            private_from: None,
            cleared: false,
            engine_block: 0.0,
            stats: MouthStats::default(),
        }
    }

    /// The lead the pump should keep, seconds.
    pub fn lead(&self) -> Seconds {
        Seconds(self.lead)
    }

    /// The counters so far.
    pub fn stats(&self) -> MouthStats {
        self.stats
    }

    /// Block `k`'s key in the current reply, if the model decided it: its voice time and values. Pre-roll blocks
    /// have none.
    pub fn key(&self, k: u64) -> Option<(Seconds, ChannelBuf)> {
        self.gens[self.current].key(k).map(|values| (Seconds(self.key_v(k)), *values))
    }

    /// Whether the reply has a head key at voice time 0: the model's latency is not a whole number of blocks.
    pub fn has_head_key(&self) -> bool {
        self.warm * self.b() > u64::from(self.spec.latency)
    }

    fn f(&self) -> f64 {
        f64::from(self.spec.sample_rate.max(1))
    }

    fn b(&self) -> u64 {
        u64::from(self.spec.block.max(1))
    }

    /// The voice time of block `k`'s key: `(kB − L)/f`.
    #[allow(clippy::cast_precision_loss)]
    fn key_v(&self, k: u64) -> f64 {
        ((k * self.b()) as f64 - f64::from(self.spec.latency)) / self.f()
    }

    /// A new feed starts (§4.1): the probe the next reply is released against.
    pub fn attach(&mut self, probe: CutProbe) {
        self.start_generation(Some(probe));
    }

    /// The pump saw a cut at `at`: the next PCM starts a new generation. Release decisions come from the probe.
    pub fn cut(&mut self, _at: Seconds) {
        let g = &mut self.gens[self.current];
        g.sealed = true;
    }

    fn start_generation(&mut self, probe: Option<CutProbe>) {
        let old = self.current;
        let next = 1 - old;
        // The retiring generation keeps releasing what already played; one older than that is gone.
        self.current = next;
        let same_feed = match (&probe, &self.gens[old].probe) {
            (Some(a), Some(b)) => a.same(b),
            _ => false,
        };
        let cursor = if same_feed { self.gens[old].record_cursor } else { 0 };
        self.gens[next].reset(probe);
        self.gens[next].record_cursor = cursor;
        self.gens[old].sealed = true;
        if self.gens[old].pushed > 0 || self.gens[old].live {
            self.sink.restart();
        }
        self.stats.generations += 1;
    }

    /// Lead PCM: `pcm` at `rate` Hz, written at feed index `feed_sample`, predicted to play from `at`. Copies and
    /// returns. Allocates nothing.
    pub fn push(&mut self, at: Seconds, feed_sample: u64, rate: u32, pcm: &[f32], private: bool) {
        if private {
            if self.private_from.is_none() {
                self.private_from = Some(at.0);
                self.cleared = false;
            }
            return;
        }
        self.private_from = None;
        let g = &self.gens[self.current];
        if !g.live || g.sealed || g.closed {
            let probe = g.probe.clone();
            self.start_generation(probe);
        }
        let f = self.spec.sample_rate;
        self.gens[self.current].append(at.0, feed_sample, rate, pcm, f);
    }

    /// Runs the drive at program time `now`: pushes the blocks that are due to the model, takes the keys, and hands
    /// each item that may go to the stage to `out` as `(stamp, layer)`, stamps strictly rising. Allocates nothing.
    ///
    /// Release comes first: a tick is evaluated from the keys decided before this instant, so a key the model
    /// answers at the moment its tick is due counts as late (§6.3), and the model's hand-off follows.
    pub fn poll(&mut self, now: Seconds, out: &mut dyn FnMut(Seconds, &Layer)) {
        let now = now.0;
        self.release_all(now, out);
        self.feed_model(now);
    }

    fn release_all(&mut self, now: f64, out: &mut dyn FnMut(Seconds, &Layer)) {
        let retiring = 1 - self.current;
        if self.gens[retiring].live {
            if self.release(retiring, now, out) {
                return; // a cut is raised: nothing goes until the audio thread says where
            }
            if self.gens[retiring].pending() {
                // Its audio plays first: nothing of the current generation goes before it.
                self.release_fade(now, out);
                return;
            }
        }
        if self.release(self.current, now, out) {
            return;
        }
        self.release_private(now, out);
        self.release_fade(now, out);
    }

    // ---- the model ----

    /// Pushes every block whose pacing time has come, taking the key `queue` blocks back with each.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn feed_model(&mut self, now: f64) {
        let cur = self.current;
        let b = self.b();
        loop {
            let g = &self.gens[cur];
            if !g.live || g.closed || g.tail == Some(0) || g.segment_count == 0 {
                return;
            }
            let k = g.pushed;
            let end = (k + 1) * b;
            if now < g.at_of(end as f64 / self.f()) - self.lead {
                return;
            }
            let complete = g.model_len >= end;
            let g = &mut self.gens[cur];
            if !complete && g.tail.is_none() {
                // The voice ran out a block before this one was due: flush the model with silence.
                let real = g.model_len > k * b;
                g.tail = Some(u32::try_from(self.queue + self.warm).unwrap_or(u32::MAX) + u32::from(real));
                g.sealed = true;
            }
            let from = (k * b) as i64;
            g.copy(from, from + b as i64, &mut self.block);
            if let Some(left) = g.tail.as_mut() {
                *left = left.saturating_sub(1);
            }
            let input = [TensorRef::f32(&self.block, &self.shape)];
            let Ok(seq) = self.sink.push(&input) else { return };
            let g = &mut self.gens[cur];
            let seq0 = *g.seq0.get_or_insert(seq);
            if seq != seq0 + k {
                // The stream counted differently (a restart elsewhere): keep our numbering on its sequence.
                g.seq0 = Some(seq - k);
            }
            g.pushed += 1;
            if k >= self.queue {
                self.take(cur, k - self.queue);
            }
        }
    }

    /// Takes block `k`'s result and decides its key.
    #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
    fn take(&mut self, cur: usize, k: u64) {
        let Some(seq0) = self.gens[cur].seq0 else { return };
        let block = self.source.take(seq0 + k, &mut self.outputs);
        self.gens[cur].taken = k + 1;
        if k < self.warm {
            return; // pre-roll
        }
        let stalled = self.source.stats().stalled;
        let step = (self.b() as f64 / self.f() / CROSSFADE_S) as f32;
        let mut viseme = ChannelBuf::NEUTRAL;
        self.viseme_of(cur, k, &mut viseme);
        let g = &mut self.gens[cur];
        let mut values = viseme;
        match (block, self.outputs.first().and_then(TensorBuf::as_f32)) {
            (Block::Ready, Some(model)) if !stalled => {
                g.w = (g.w + step).min(1.0);
                for (i, &m) in model.iter().take(ARKIT_COUNT).enumerate() {
                    if let Some(channel) = Channel::from_index(i) {
                        g.last_model.set(channel, m);
                    }
                }
                values.lerp_masked(&g.last_model, self.config.mask, g.w);
            }
            _ => {
                g.w = if stalled { 0.0 } else { (g.w - step).max(0.0) };
                self.stats.missed_keys += 1;
            }
        }
        g.keys[(k % KEY_RING as u64) as usize] = Key { k, values };
    }

    /// The viseme frame of block `k`'s depicted interval `[kB − L, (k+1)B − L)`.
    #[allow(clippy::cast_possible_wrap)]
    fn viseme_of(&mut self, cur: usize, k: u64, out: &mut ChannelBuf) {
        let (b, l) = (self.b() as i64, i64::from(self.spec.latency));
        let from = k as i64 * b - l;
        self.viseme_range(cur, from, from + b, out);
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn viseme_range(&mut self, cur: usize, from: i64, to: i64, out: &mut ChannelBuf) {
        let g = &self.gens[cur];
        // Only what the voice holds: before it and after it is silence, a closed mouth.
        let from = from.max(0);
        let to = to.min(g.model_len as i64);
        let pcm = g.copy(from, to.max(from), &mut self.scratch);
        self.visemes.frame(pcm, self.spec.sample_rate, out);
    }

    // ---- ticks ----

    /// The curve at voice time `v`: the keys around it, interpolated. Returns whether a key was not decided yet.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn curve(&mut self, cur: usize, v: f64, out: &mut ChannelBuf) -> bool {
        let (b, l) = (self.b() as f64, f64::from(self.spec.latency));
        let u = v * self.f();
        let head = self.warm * self.b() > u64::from(self.spec.latency);
        let first_block_u = self.warm as f64 * b - l;
        let mut late = false;
        let mut left = ChannelBuf::NEUTRAL;
        // Left key and its place.
        let right_k = (((u + l) / b - 1e-9).ceil().max(0.0) as u64).max(self.warm);
        let right_u = right_k as f64 * b - l;
        late |= self.key_value(cur, right_k, out);
        if u >= right_u - 1e-9 || (u <= first_block_u && !head) {
            return late;
        }
        let left_u = if right_k > self.warm {
            late |= self.key_value(cur, right_k - 1, &mut left);
            right_u - b
        } else {
            // The head key: the viseme of the pre-roll interval [0, WB − L).
            self.viseme_range(cur, 0, first_block_u as i64, &mut left);
            0.0
        };
        let alpha = ((u - left_u) / (right_u - left_u)).clamp(0.0, 1.0) as f32;
        let right = *out;
        *out = left;
        out.lerp_masked(&right, self.config.mask, alpha);
        late
    }

    /// Block `k`'s key into `out`; a key not decided yet is its viseme frame, and `true` comes back. A key wholly
    /// past the voice is rest, never late.
    #[allow(clippy::cast_precision_loss)]
    fn key_value(&mut self, cur: usize, k: u64, out: &mut ChannelBuf) -> bool {
        if let Some(values) = self.gens[cur].key(k) {
            *out = *values;
            return false;
        }
        self.viseme_of(cur, k, out);
        let g = &self.gens[cur];
        let starts_after_voice = self.key_v(k) >= g.voice - 1e-9;
        !starts_after_voice
    }

    fn layer(&self, values: &ChannelBuf) -> Layer {
        let mut layer = Layer::absent(BlendMode::Max);
        layer.values.copy_masked(values, self.config.mask);
        layer.mask = self.config.mask;
        layer.weight = 1.0;
        layer
    }

    /// Hands one item to the stage, after settling the fade before it.
    fn emit(&mut self, p: f64, values: &ChannelBuf, clear: bool, now: f64, out: &mut dyn FnMut(Seconds, &Layer)) {
        if let Some(fade) = self.fade {
            if fade.close {
                // Close items before this one that are due go first (only the newest of them); the rest are dropped:
                // the new reply wins a tie.
                self.release_fade_due(now, p, out);
            }
            self.fade = None;
        }
        if p.partial_cmp(&self.last_p) != Some(std::cmp::Ordering::Greater) {
            self.stats.refused += 1;
            return;
        }
        let layer = if clear { Layer::absent(BlendMode::Max) } else { self.layer(values) };
        out(Seconds(p - self.config.delay.0), &layer);
        self.last_p = p;
        self.last_values = *values;
        self.last_clear = clear;
    }

    /// Releases the ticks of generation `cur` whose audio played. Returns `true` while its cut is raised: then
    /// nothing at all may be released.
    #[allow(clippy::cast_precision_loss)]
    fn release(&mut self, cur: usize, now: f64, out: &mut dyn FnMut(Seconds, &Layer)) -> bool {
        let Some(probe) = self.gens[cur].probe.clone() else { return false };
        if self.gens[cur].closed {
            return false;
        }
        let cutoff = match probe.state() {
            CutState::Live => f64::INFINITY,
            CutState::Cut { at, .. } => at.0,
            _ => return true,
        };
        loop {
            let g = &self.gens[cur];
            let v = g.next_tick as f64 * TICK_S;
            if v >= g.voice - 1e-9 {
                break;
            }
            let Some(sample) = g.feed_of(v) else {
                self.gens[cur].next_tick += 1;
                self.stats.history_lost += 1;
                continue;
            };
            let cursor = self.gens[cur].record_cursor;
            match self.find(cur, &probe, sample) {
                // Known, but not playing yet (a record is published as its block starts): it waits for its time.
                Found::At(p) if p > now + 1e-9 && p <= cutoff => break,
                Found::At(p) if p <= cutoff => {
                    if let Some(g0) = self.gens[cur].stall.take() {
                        self.resume(cur, &probe, cursor, g0, v, now, out);
                    }
                    let mut values = ChannelBuf::NEUTRAL;
                    if self.curve(cur, v, &mut values) {
                        self.stats.late_keys += 1;
                    }
                    self.emit(p, &values, false, now, out);
                    self.stats.ticks += 1;
                    self.gens[cur].next_tick += 1;
                }
                // Past the cut: it never played.
                Found::At(_) => break,
                Found::Lost => {
                    self.gens[cur].next_tick += 1;
                    self.stats.history_lost += 1;
                }
                Found::NotYet => {
                    if cutoff.is_infinite() {
                        if let Some(g0) = self.gens[cur].stall {
                            // The audio may have resumed short of this tick: the resume tick goes now.
                            if self.resume(cur, &probe, cursor, g0, v, now, out) {
                                self.gens[cur].stall = None;
                            }
                        } else {
                            self.check_stall(cur, &probe, now);
                        }
                    }
                    break;
                }
            }
        }
        if cutoff.is_finite() {
            self.close_cut(cur, cutoff);
        } else {
            self.check_end(cur, &probe);
        }
        false
    }

    /// Where feed sample `sample` played, from its own record.
    fn find(&mut self, cur: usize, probe: &CutProbe, sample: u64) -> Found {
        let head = probe.head();
        let mut i = self.gens[cur].record_cursor;
        while i < head {
            match probe.record(i) {
                Ok(r) => {
                    self.engine_block = self.engine_block.max(r.end.0 - r.start.0);
                    if r.holds(sample) {
                        self.gens[cur].record_cursor = i;
                        return r.time_of(sample).map_or(Found::Lost, |t| Found::At(t.0));
                    }
                    if r.consumed > sample {
                        // Its record was reused before it was read.
                        self.gens[cur].record_cursor = i;
                        return Found::Lost;
                    }
                }
                Err(PlayGap::Lapped) => {}
                Err(PlayGap::NotYet) => break,
            }
            i += 1;
        }
        Found::NotYet
    }

    /// The head tick's audio has not played: a stall when the log has been silent for a block.
    fn check_stall(&mut self, cur: usize, probe: &CutProbe, now: f64) {
        if self.gens[cur].stall.is_some() || self.last_clear {
            return;
        }
        let Some(latest) = probe.head().checked_sub(1).and_then(|i| probe.record(i).ok()) else { return };
        let g0 = latest.end.0;
        let block = if self.engine_block > 0.0 { self.engine_block } else { DEFAULT_ENGINE_BLOCK_S };
        if g0 <= self.last_p || now < g0 + block - 1e-9 {
            return;
        }
        self.gens[cur].stall = Some(g0);
        self.fade = Some(Fade { close: false, start: g0, from: self.last_values, next: 1 });
    }

    /// If the audio resumed after a stall that began at `g0` (a record from `cursor` on starts at or after it): drop
    /// the fade items not released and land a resume tick where the audio did, with the curve at the voice time it
    /// resumed from. Returns whether it had resumed.
    #[allow(clippy::too_many_arguments)]
    fn resume(
        &mut self,
        cur: usize,
        probe: &CutProbe,
        cursor: u64,
        g0: f64,
        head_v: f64,
        now: f64,
        out: &mut dyn FnMut(Seconds, &Layer),
    ) -> bool {
        let head = probe.head();
        for i in cursor..head {
            if let Ok(r) = probe.record(i)
                && r.start.0 >= g0 - 1e-9
            {
                self.fade = None;
                let v = self.gens[cur].voice_of(r.consumed).unwrap_or(head_v);
                if v < head_v - 1e-9 {
                    let mut values = ChannelBuf::NEUTRAL;
                    self.curve(cur, v, &mut values);
                    self.emit(r.start.0, &values, false, now, out);
                }
                return true;
            }
        }
        false
    }

    /// The probe reported a cut at `t`: what played by then went out; drop the rest and close the mouth.
    #[allow(clippy::cast_precision_loss)]
    fn close_cut(&mut self, cur: usize, t: f64) {
        let g = &mut self.gens[cur];
        let dropped = ((g.voice / TICK_S).ceil() as u64).saturating_sub(g.next_tick);
        self.stats.cut_ticks += dropped;
        g.closed = true;
        g.sealed = true;
        g.stall = None;
        if cur == self.current {
            self.sink.restart();
            self.gens[cur].tail = Some(0);
        }
        self.start_close(t);
    }

    /// All ticks went and the feed played past the voice: the reply ended, so the mouth closes from where it did.
    fn check_end(&mut self, cur: usize, probe: &CutProbe) {
        let g = &self.gens[cur];
        if g.ended || g.pending() || g.next_tick == 0 || g.segment_count == 0 {
            return;
        }
        let last = *g.segment(g.segment_count - 1);
        let after = last.feed + last.len;
        let Some(latest) = probe.head().checked_sub(1).and_then(|i| probe.record(i).ok()) else { return };
        if latest.consumed + u64::from(latest.n) < after {
            return;
        }
        // The program time just after the voice's last sample, from the record that played it.
        let mut end = latest.end.0;
        for i in (self.gens[cur].record_cursor..probe.head()).rev() {
            if let Ok(r) = probe.record(i)
                && let Some(t) = r.time_of(after - 1)
            {
                end = t.0 + (r.end.0 - r.start.0) / f64::from(r.n);
                break;
            }
        }
        self.gens[cur].ended = true;
        self.start_close(end);
    }

    fn start_close(&mut self, t: f64) {
        if self.last_clear {
            self.fade = None;
            return;
        }
        let start = t.max(self.last_p + EPSILON_S);
        self.fade = Some(Fade { close: true, start, from: self.last_values, next: 1 });
    }

    /// Releases the newest fade item that is due, collapsing older due ones.
    fn release_fade(&mut self, now: f64, out: &mut dyn FnMut(Seconds, &Layer)) {
        self.release_fade_due(now, f64::INFINITY, out);
    }

    /// Releases the newest fade item due by `now` with a time before `before`; older due ones are dropped (counted
    /// in `fade_overdue`), so the mouth jumps to where the fade is and never replays what it missed.
    #[allow(clippy::cast_precision_loss)]
    fn release_fade_due(&mut self, now: f64, before: f64, out: &mut dyn FnMut(Seconds, &Layer)) {
        let Some(mut fade) = self.fade else { return };
        let mut due = None;
        while fade.next <= FADE_STEPS {
            let t = fade.start + TICK_S * f64::from(fade.next);
            if t > now + 1e-9 || t >= before - 1e-9 {
                break;
            }
            if due.is_some() {
                self.stats.fade_overdue += 1;
            }
            due = Some((fade.next, t));
            fade.next += 1;
        }
        self.fade = (fade.next <= FADE_STEPS).then_some(fade);
        let Some((k, t)) = due else { return };
        let keep = 1.0 - k as f32 / FADE_STEPS as f32;
        let mut values = ChannelBuf::NEUTRAL;
        for c in self.config.mask {
            values.set(c, fade.from.get(c) * keep);
        }
        if t > self.last_p {
            let layer = self.layer(&values);
            out(Seconds(t - self.config.delay.0), &layer);
            self.last_p = t;
            self.last_values = values;
            self.last_clear = false;
        } else {
            self.stats.refused += 1;
        }
    }

    /// Private speech clears the layer when it starts playing.
    fn release_private(&mut self, now: f64, out: &mut dyn FnMut(Seconds, &Layer)) {
        let Some(from) = self.private_from else { return };
        if self.cleared || now < from || self.gens[self.current].pending() {
            return;
        }
        self.fade = None;
        let p = from.max(self.last_p + EPSILON_S);
        self.emit(p, &ChannelBuf::NEUTRAL, true, now, out);
        self.cleared = true;
    }
}

/// A tick's audio, as the play log knows it.
enum Found {
    /// Played at this program time.
    At(f64),
    /// Not played yet.
    NotYet,
    /// Its record was reused before it was read.
    Lost,
}
