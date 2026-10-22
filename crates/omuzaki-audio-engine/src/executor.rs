//! The real-time executor: the compiled mixer graph and everything the audio thread touches.
//!
//! The executor runs on the clock device's callback (or the timer, when no device clocks the graph), under a
//! `Mutex::try_lock` that only the audio side takes, so it never waits. The control thread reaches it only through
//! [`Command`]s on a bounded channel, read at block starts; anything the audio thread lets go of (an old graph, a closed
//! tap) goes back to the control side as [`Garbage`] and is freed there, so the audio thread never frees.
//!
//! Per block of at most `MAX_BLOCK` frames:
//! 1. each bridge (one per bound input) yields the block's frames, or silence on an underrun;
//! 2. each feed yields its frames, resampled to 48 kHz at the edge;
//! 3. the mixer core ([`Mix`](crate::mix::Mix)) runs the buses: sources and sends, chains, faders, mutes, ducking and
//!    the outputs, exactly as the offline render does;
//! 4. every tap at a point reached copies the block out (mixed down, decimated, or silenced by the mic mode).
//!
//! What is left for later, on purpose: outputs to devices are not played; a structural graph change restarts the
//! nodes and ramps instead of carrying their state over (#908); players are silent until the player source (#1160).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_audio_api::clock::{ClockSnapshot, EngineClock};
use omuzaki_audio_api::graph::{MicMode, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId, GateId};
use omuzaki_audio_api::node::{MAX_BLOCK, ParamIndex, SAMPLE_RATE};
use omuzaki_audio_api::player::Sound;
use omuzaki_audio_api::report::MeterCell;
use omuzaki_audio_api::ring::{Consumer, Producer, TapFlags};
use omuzaki_audio_api::semantics::MONO_TO_STEREO_GAIN;
use omuzaki_audio_api::tap::{TapClosed, TapPoint, TapSink, VoiceUse};
use omuzaki_core::time::Seconds;

use crate::dsp::{Decimator, Resampler};
use crate::meters::Meter;
use crate::mix::{Inputs, Mix, Points, Stage};
use crate::player::{Bank, MAX_PLAYERS, Player, held_flags};
use crate::swap::Running;

/// The most taps, feeds and bridges the executor holds; their lists are allocated this large up front.
pub const MAX_ENTRIES: usize = 64;
/// Commands applied per block at most, so a flood never stalls a block.
const COMMANDS_PER_BLOCK: usize = 64;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counters the control side reads.
#[derive(Debug)]
pub(crate) struct Shared {
    /// Per bank slot, whether the audio thread holds a sound there.
    pub held: Arc<[AtomicBool]>,
    /// The engine-clock index of the next frame the executor will make.
    pub sample: AtomicU64,
    /// Frames in the last block the clock delivered.
    pub block_frames: AtomicU32,
    /// Callbacks that found the executor busy and skipped a block.
    pub missed: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            held: held_flags(),
            sample: AtomicU64::new(0),
            block_frames: AtomicU32::new(0),
            missed: AtomicU64::new(0),
        }
    }
}

/// A device input crossing from its own callback to the executor, one ring per channel.
pub(crate) struct Bridge {
    pub token: u64,
    pub role: DeviceRole,
    pub rings: Vec<Consumer>,
    pub block: [Vec<f32>; 2],
    pub underruns: Arc<AtomicU64>,
    pub meter: Arc<MeterCell>,
    pub meter_state: Meter,
    /// Set by the device side once it has delivered audio: before that an empty ring is not an underrun.
    pub started: Arc<AtomicBool>,
}

/// A feed into a bus.
pub(crate) struct Feed {
    pub id: FeedId,
    pub ring: Consumer,
    pub stereo: bool,
    pub rate: u32,
    pub block: [Vec<f32>; 2],
    /// Pull resampling state: the last two input frames and the position between them.
    pub pull: PullState,
    pub staging: Vec<f32>,
}

/// Linear pull resampling of a feed, both channels on one position.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PullState {
    last: [f32; 2],
    next: [f32; 2],
    position: f64,
}

/// Where a tap's samples go.
pub(crate) enum TapLink {
    Ring(Producer),
    Sink(Box<dyn TapSink>, TapClosed),
}

/// One open tap.
pub(crate) struct Tap {
    /// The resolved point (never `TapPoint::Voice`).
    pub point: TapPoint,
    pub voice: Option<VoiceUse>,
    pub mono: bool,
    pub speech: bool,
    pub decimator: Decimator,
    pub link: TapLink,
    pub flags: TapFlags,
    pub scratch: Vec<f32>,
    pub dropped: Arc<AtomicU64>,
}

impl Tap {
    fn is_closed(&self) -> bool {
        match &self.link {
            TapLink::Ring(p) => p.reader_gone(),
            TapLink::Sink(_, closed) => closed.is_closed(),
        }
    }
}

/// What the mic mode does to a voice consumer (design section 4.2): `Muted` silences all of them, `CohostOnly` all but
/// `listen`, which stays open and is flagged private.
pub(crate) fn voice_flags(mode: MicMode, consumer: VoiceUse) -> TapFlags {
    let listens = consumer.hears_private_talk();
    TapFlags {
        muted: match mode {
            MicMode::Live => false,
            MicMode::Muted => true,
            MicMode::CohostOnly => !listens,
        },
        private: mode == MicMode::CohostOnly && listens,
    }
}

/// A message from the control side, applied at the next block start.
pub(crate) enum Command {
    Swap(Box<Mix>),
    Gain {
        generation: u64,
        bus: usize,
        gain: f32,
    },
    Mute {
        generation: u64,
        bus: usize,
        muted: bool,
    },
    Param {
        generation: u64,
        bus: usize,
        node: usize,
        param: ParamIndex,
        value: f32,
    },
    MicMode(MicMode),
    AddTap(Box<Tap>),
    AddFeed(Box<Feed>),
    AddBridge(Box<Bridge>),
    RemoveBridge(u64),
    /// A sound into a bank slot; the control side keeps a reference, so the audio thread never frees it.
    LoadSound {
        slot: usize,
        sound: Arc<Sound>,
    },
    UnloadSound {
        slot: usize,
    },
    /// A player the control side numbered by its position: players are only ever added.
    AddPlayer(Box<Player>),
    Play {
        player: usize,
        slot: usize,
        gain: f32,
        ducks: bool,
    },
    Stop {
        player: usize,
        slot: Option<usize>,
    },
}

/// What the audio thread hands back to be freed off it.
pub(crate) enum Garbage {
    Graph(#[allow(dead_code)] Box<Mix>),
    Tap(#[allow(dead_code)] Box<Tap>),
    Feed(#[allow(dead_code)] Box<Feed>),
    Bridge(#[allow(dead_code)] Box<Bridge>),
    Player(#[allow(dead_code)] Box<Player>),
    Sound(#[allow(dead_code)] Arc<Sound>),
}

/// The executor's state.
// Boxed entries move between threads as one pointer, so adding or retiring one never copies its buffers.
#[allow(clippy::vec_box)]
pub(crate) struct Executor {
    running: Running,
    bridges: Vec<Box<Bridge>>,
    feeds: Vec<Box<Feed>>,
    taps: Vec<Box<Tap>>,
    bank: Bank,
    players: Vec<Box<Player>>,
    mic_mode: MicMode,
    commands: Receiver<Command>,
    garbage: SyncSender<Garbage>,
    /// Things that could not be handed back yet because the garbage channel was full.
    held: Vec<Garbage>,
    clock: Arc<EngineClock>,
    shared: Arc<Shared>,
    sample: u64,
    /// The smoothed engine time of `sample`, from the clock device's timestamps.
    time: Option<f64>,
}

fn zero_block(n: usize) -> [Vec<f32>; 2] {
    [vec![0.0; n], vec![0.0; n]]
}

impl Executor {
    pub(crate) fn new(
        graph: Box<Mix>,
        commands: Receiver<Command>,
        garbage: SyncSender<Garbage>,
        clock: Arc<EngineClock>,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            running: Running::new(graph),
            bridges: Vec::with_capacity(MAX_ENTRIES),
            feeds: Vec::with_capacity(MAX_ENTRIES),
            taps: Vec::with_capacity(MAX_ENTRIES),
            bank: Bank::new(Arc::clone(&shared.held)),
            players: Vec::with_capacity(MAX_PLAYERS),
            mic_mode: MicMode::Live,
            commands,
            garbage,
            held: Vec::with_capacity(4 * MAX_ENTRIES),
            clock,
            shared,
            sample: 0,
            time: None,
        }
    }

    fn discard(&mut self, g: Garbage) {
        if let Err(std::sync::mpsc::TrySendError::Full(g) | std::sync::mpsc::TrySendError::Disconnected(g)) =
            self.garbage.try_send(g)
        {
            // `held` was allocated for every entry the executor can own, so this push does not grow it in practice.
            self.held.push(g);
        }
    }

    fn apply_commands(&mut self) {
        while let Some(g) = self.held.pop() {
            if let Err(std::sync::mpsc::TrySendError::Full(g) | std::sync::mpsc::TrySendError::Disconnected(g)) =
                self.garbage.try_send(g)
            {
                self.held.push(g);
                break;
            }
        }
        for _ in 0..COMMANDS_PER_BLOCK {
            let Ok(command) = self.commands.try_recv() else { return };
            match command {
                Command::Swap(mut graph) => {
                    // Compiled under the mode the control side knew; a mode change since then ramps, as it would live.
                    graph.set_mic_mode(self.mic_mode);
                    if let Some(superseded) = self.running.offer(graph) {
                        self.discard(Garbage::Graph(superseded));
                    }
                }
                Command::Gain { generation, bus, gain } => {
                    if let Some(graph) = self.running.generation(generation) {
                        graph.set_gain(bus, gain);
                    }
                }
                Command::Mute { generation, bus, muted } => {
                    if let Some(graph) = self.running.generation(generation) {
                        graph.set_muted(bus, muted);
                    }
                }
                Command::Param { generation, bus, node, param, value } => {
                    if let Some(link) = self
                        .running
                        .generation(generation)
                        .and_then(|g| g.buses.get_mut(bus))
                        .and_then(|b| b.chain.get_mut(node))
                    {
                        link.node.set_param(param, value);
                    }
                }
                Command::MicMode(mode) => {
                    self.mic_mode = mode;
                    self.running.set_mic_mode(mode);
                }
                Command::AddTap(tap) => push_bounded(&mut self.taps, tap, Garbage::Tap, &mut self.held),
                Command::AddFeed(feed) => push_bounded(&mut self.feeds, feed, Garbage::Feed, &mut self.held),
                Command::AddBridge(bridge) => {
                    push_bounded(&mut self.bridges, bridge, Garbage::Bridge, &mut self.held);
                }
                Command::LoadSound { slot, sound } => {
                    if let Some(old) = self.bank.load(slot, sound) {
                        self.discard(Garbage::Sound(old));
                    }
                }
                Command::UnloadSound { slot } => {
                    self.bank.unload(slot);
                    for player in &mut self.players {
                        player.stop(Some(slot));
                    }
                }
                Command::AddPlayer(player) => {
                    if self.players.len() < MAX_PLAYERS {
                        self.players.push(player);
                    } else {
                        self.discard(Garbage::Player(player));
                    }
                }
                Command::Play { player, slot, gain, ducks } => {
                    if let Some(player) = self.players.get_mut(player) {
                        player.play(slot, gain, ducks);
                    }
                }
                Command::Stop { player, slot } => {
                    if let Some(player) = self.players.get_mut(player) {
                        player.stop(slot);
                    }
                }
                Command::RemoveBridge(token) => {
                    if let Some(at) = self.bridges.iter().position(|b| b.token == token) {
                        let bridge = self.bridges.swap_remove(at);
                        self.discard(Garbage::Bridge(bridge));
                    }
                }
            }
        }
    }

    /// Makes `frames` frames. `time` is the engine time of the first one as the clock device stamped it (`None` from
    /// the timer, which continues the timeline from the frame count).
    pub(crate) fn run(&mut self, frames: usize, time: Option<Seconds>) {
        let period = 1.0 / f64::from(SAMPLE_RATE);
        // A one-pole fit of the device's timestamps: stamps jitter by a buffer; the fit keeps time smooth and monotonic.
        #[allow(clippy::cast_precision_loss)] // Frame counts per call: small.
        let smoothed = match (self.time, time) {
            (None, Some(t)) => t.0,
            (Some(prev), Some(t)) => prev + 0.05 * (t.0 - prev),
            (Some(prev), None) => prev,
            (None, None) => 0.0,
        };
        self.clock.publish(ClockSnapshot { sample: self.sample, time: Seconds(smoothed), seconds_per_sample: period });
        let mut left = frames;
        while left > 0 {
            let n = left.min(MAX_BLOCK);
            self.block(n);
            left -= n;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.time = Some(smoothed + frames as f64 * period);
        }
        self.shared.block_frames.store(u32::try_from(frames).unwrap_or(u32::MAX), Ordering::Relaxed);
    }

    #[allow(clippy::too_many_lines)] // One block's whole signal flow, in order; splitting it would hide the order.
    fn block(&mut self, n: usize) {
        self.apply_commands();
        for bridge in &mut self.bridges {
            let started = bridge.started.load(Ordering::Acquire);
            for (ch, ring) in bridge.rings.iter_mut().enumerate().take(2) {
                let got = ring.pop_into(&mut bridge.block[ch][..n]);
                if got < n {
                    bridge.block[ch][got..n].fill(0.0);
                    if started && ch == 0 {
                        bridge.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            let channels = bridge.rings.len().min(2);
            let reading = match channels {
                2 => bridge.meter_state.measure(&[&bridge.block[0][..n], &bridge.block[1][..n]], 0.0),
                _ => bridge.meter_state.measure(&[&bridge.block[0][..n]], 0.0),
            };
            bridge.meter.store(reading);
        }
        for feed in &mut self.feeds {
            pull_feed(feed, n);
        }
        for player in &mut self.players {
            player.render(n, &self.bank);
        }
        while let Some(sound) = self.bank.take_released(&self.players) {
            self.discard(Garbage::Sound(sound));
        }
        // Input taps.
        for tap in &mut self.taps {
            if let TapPoint::Input(role) = &tap.point {
                let found = self.bridges.iter().find(|b| &b.role == role);
                match found {
                    Some(b) if b.rings.len() >= 2 => {
                        feed_tap(tap, &[&b.block[0][..n], &b.block[1][..n]], self.mic_mode);
                    }
                    Some(b) => feed_tap(tap, &[&b.block[0][..n]], self.mic_mode),
                    None => feed_silence(tap, n, self.mic_mode),
                }
            }
        }
        let first_sample = self.sample;
        let mic_mode = self.mic_mode;
        let inputs = LiveInputs { bridges: &self.bridges, feeds: &self.feeds, players: &self.players };
        let mut points = LiveTaps { taps: &mut self.taps, mode: mic_mode };
        if let Some(old) = self.running.process(n, first_sample, &inputs, &mut points) {
            self.discard(Garbage::Graph(old));
        }
        self.sample += n as u64;
        self.shared.sample.store(self.sample, Ordering::Relaxed);
        self.prune();
    }

    /// Hands closed taps, feeds whose writer is gone, and nothing else back to the control side.
    fn prune(&mut self) {
        let mut i = 0;
        while i < self.taps.len() {
            if self.taps[i].is_closed() {
                let tap = self.taps.swap_remove(i);
                self.discard(Garbage::Tap(tap));
            } else {
                i += 1;
            }
        }
        let mut i = 0;
        while i < self.feeds.len() {
            if self.feeds[i].ring.writer_gone() && self.feeds[i].ring.available() == 0 {
                let feed = self.feeds.swap_remove(i);
                self.discard(Garbage::Feed(feed));
            } else {
                i += 1;
            }
        }
    }
}

fn push_bounded<T>(
    list: &mut Vec<Box<T>>,
    item: Box<T>,
    wrap: impl FnOnce(Box<T>) -> Garbage,
    held: &mut Vec<Garbage>,
) {
    if list.len() < MAX_ENTRIES {
        list.push(item);
    } else {
        // The control side refuses past the limit; this is the belt to those braces.
        held.push(wrap(item));
    }
}

/// The bridges, feeds and players as the mixer's sources, and the players' gates.
struct LiveInputs<'a> {
    bridges: &'a [Box<Bridge>],
    feeds: &'a [Box<Feed>],
    players: &'a [Box<Player>],
}

impl Inputs for LiveInputs<'_> {
    fn source(&self, source: &SourceRef, n: usize) -> Option<(usize, [&[f32]; 2])> {
        match source {
            SourceRef::Device { role } => {
                let bridge = self.bridges.iter().find(|b| &b.role == role)?;
                let channels = bridge.rings.len().min(2);
                (channels > 0).then(|| (channels, [&bridge.block[0][..n], &bridge.block[1][..n]]))
            }
            SourceRef::Feed { id } => {
                let feed = self.feeds.iter().find(|f| &f.id == id)?;
                Some((if feed.stereo { 2 } else { 1 }, [&feed.block[0][..n], &feed.block[1][..n]]))
            }
            SourceRef::Player { id } => {
                let player = self.players.iter().find(|p| &p.id == id)?;
                Some((2, [&player.out[0][..n], &player.out[1][..n]]))
            }
        }
    }

    fn gate(&self, gate: &GateId, n: usize) -> Option<&[bool]> {
        // A player raises the gate with its own name.
        let player = self.players.iter().find(|p| p.id.as_str() == gate.as_str())?;
        Some(&player.gate[..n])
    }
}

/// The open taps as the mixer's points.
struct LiveTaps<'a> {
    taps: &'a mut [Box<Tap>],
    mode: MicMode,
}

impl Points for LiveTaps<'_> {
    /// Taps on a bus this graph does not have read silence, so their positions stay contiguous.
    fn ran(&mut self, mix: &Mix, frames: usize) {
        for tap in self.taps.iter_mut() {
            let missing = match &tap.point {
                TapPoint::BusPreChain(id) | TapPoint::BusPostChain(id) | TapPoint::BusPostFader(id) => {
                    !mix.buses.iter().any(|b| &b.id == id)
                }
                _ => false,
            };
            if missing {
                feed_silence(tap, frames, self.mode);
            }
        }
    }

    fn reached(&mut self, stage: Stage, bus: &BusId, channels: usize, block: [&[f32]; 2]) {
        for tap in self.taps.iter_mut() {
            let here = match (&tap.point, stage) {
                (TapPoint::BusPreChain(id), Stage::PreChain)
                | (TapPoint::BusPostChain(id), Stage::PostChain)
                | (TapPoint::BusPostFader(id), Stage::PostFader) => id == bus,
                _ => false,
            };
            if !here {
                continue;
            }
            if channels == 2 {
                feed_tap(tap, &block, self.mode);
            } else {
                feed_tap(tap, &block[..1], self.mode);
            }
        }
    }
}

fn tap_flags(tap: &Tap, mode: MicMode) -> TapFlags {
    tap.voice.map_or_else(TapFlags::default, |consumer| voice_flags(mode, consumer))
}

fn feed_silence(tap: &mut Tap, n: usize, mode: MicMode) {
    let zeros = [0.0f32; MAX_BLOCK];
    feed_tap(tap, &[&zeros[..n]], mode);
}

/// Copies one block of a point into a tap: silenced when its voice consumer is muted, mixed down when asked (or for
/// 16 kHz), interleaved otherwise, decimated for 16 kHz.
fn feed_tap(tap: &mut Tap, channels: &[&[f32]], mode: MicMode) {
    let n = channels.first().map_or(0, |c| c.len());
    let flags = tap_flags(tap, mode);
    let silent = flags.muted;
    tap.scratch.clear();
    let stereo = channels.len() == 2;
    #[allow(clippy::needless_range_loop)] // Indexes one or two channels in step.
    for i in 0..n {
        let (l, r) = if silent { (0.0, 0.0) } else { (channels[0][i], if stereo { channels[1][i] } else { 0.0 }) };
        if !tap.speech && !tap.mono && stereo {
            tap.scratch.push(l);
            tap.scratch.push(r);
            continue;
        }
        let mixed = if stereo { (l + r) * MONO_TO_STEREO_GAIN } else { l };
        if tap.speech {
            if let Some(s) = tap.decimator.push(mixed) {
                tap.scratch.push(s);
            }
        } else {
            tap.scratch.push(mixed);
        }
    }
    let changed = flags != tap.flags;
    tap.flags = flags;
    let accepted = match &mut tap.link {
        TapLink::Ring(producer) => {
            if changed {
                producer.set_flags(flags);
            }
            producer.push(&tap.scratch)
        }
        TapLink::Sink(sink, _) => {
            if changed {
                sink.flags(flags);
            }
            sink.push(&tap.scratch)
        }
    };
    let refused = tap.scratch.len().saturating_sub(accepted);
    if refused > 0 {
        tap.dropped.fetch_add(refused as u64, Ordering::Relaxed);
    }
}

/// Fills a feed's block with `n` frames at 48 kHz, resampling from its rate; a short ring plays silence.
fn pull_feed(feed: &mut Feed, n: usize) {
    let channels = if feed.stereo { 2 } else { 1 };
    if feed.rate == SAMPLE_RATE {
        if channels == 1 {
            let got = feed.ring.pop_into(&mut feed.block[0][..n]);
            feed.block[0][got..n].fill(0.0);
        } else {
            let want = 2 * n;
            let got = feed.ring.pop_into(&mut feed.staging[..want]);
            feed.staging[got..want].fill(0.0);
            for i in 0..n {
                feed.block[0][i] = feed.staging[2 * i];
                feed.block[1][i] = feed.staging[2 * i + 1];
            }
        }
        return;
    }
    let step = f64::from(feed.rate) / f64::from(SAMPLE_RATE);
    for i in 0..n {
        while feed.pull.position >= 1.0 {
            feed.pull.position -= 1.0;
            let mut frame = [0.0f32; 2];
            if feed.ring.pop_into(&mut frame[..channels]) < channels {
                frame = [0.0; 2];
            }
            feed.pull.last = feed.pull.next;
            feed.pull.next = frame;
        }
        #[allow(clippy::cast_possible_truncation)] // A fraction in [0, 1).
        let t = feed.pull.position as f32;
        for ch in 0..channels {
            feed.block[ch][i] = feed.pull.last[ch] + (feed.pull.next[ch] - feed.pull.last[ch]) * t;
        }
        feed.pull.position += step;
    }
}

impl Feed {
    pub(crate) fn new(id: FeedId, ring: Consumer, stereo: bool, rate: u32) -> Self {
        Self {
            id,
            ring,
            stereo,
            rate,
            block: zero_block(MAX_BLOCK),
            pull: PullState { position: 1.0, ..PullState::default() },
            staging: vec![0.0; 2 * MAX_BLOCK],
        }
    }
}

impl Bridge {
    pub(crate) fn new(token: u64, role: DeviceRole, rings: Vec<Consumer>, started: Arc<AtomicBool>) -> Self {
        Self {
            token,
            role,
            rings,
            block: zero_block(MAX_BLOCK),
            underruns: Arc::new(AtomicU64::new(0)),
            meter: Arc::new(MeterCell::new()),
            meter_state: Meter::plain(),
            started,
        }
    }
}

/// The device side of a bridge: it lives in the input callback, picks the bound channels, resamples them to 48 kHz and
/// pushes them into the bridge's rings.
pub(crate) struct BridgeWriter {
    pub producers: Vec<Producer>,
    pub map: Vec<usize>,
    pub device_channels: usize,
    pub resamplers: Vec<Resampler>,
    pub scratch: Vec<f32>,
    pub resampled: Vec<f32>,
    pub started: Arc<AtomicBool>,
}

impl BridgeWriter {
    /// Pushes one interleaved callback buffer; returns the 48 kHz frames it made.
    pub(crate) fn write(&mut self, interleaved: &[f32]) -> usize {
        let frames = interleaved.len() / self.device_channels.max(1);
        let mut made = 0;
        for (k, (&channel, producer)) in self.map.iter().zip(self.producers.iter_mut()).enumerate() {
            self.scratch.clear();
            self.scratch.extend(
                (0..frames).map(|f| interleaved.get(f * self.device_channels + channel).copied().unwrap_or(0.0)),
            );
            self.resampled.clear();
            let resampled = &mut self.resampled;
            self.resamplers[k].process(&self.scratch, |s| resampled.push(s));
            producer.push(&self.resampled);
            made = self.resampled.len();
        }
        self.started.store(true, Ordering::Release);
        made
    }
}
