//! [`LiveEngine`]: the control side of the live engine, the [`AudioEngine`] service.
//!
//! The control side owns the graph as data, the bindings, the open device streams and the bookkeeping; it compiles
//! graphs and sends them, with every other change, to the executor as commands. A housekeeping thread frees what the
//! audio thread hands back, drives the graph from a timer while no device clocks it, and watches the streams: a stream
//! that fails is marked `Lost`, its bridge plays silence, and it is reopened every two seconds until the device is back.
//!
//! The clock device is the first input opened. Every input, the clock included, crosses into the graph through a
//! bridge; the clock's own callback then runs the executor for exactly the frames it delivered, so the clock bridge
//! never underruns, and other inputs show their drift as underruns or overruns in `status()`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_audio_api::backend::{AudioBackend, BackendError, DeviceStream, StreamConfig};
use omuzaki_audio_api::clock::{ClockSnapshot, EngineClock};
use omuzaki_audio_api::device::{Binding, DeviceId, DeviceInfo, DeviceState, Direction, ProfileStore};
use omuzaki_audio_api::engine::{AudioEngine, AudioError};
use omuzaki_audio_api::graph::{GraphSpec, MIC_BUS, MONITOR_OUT_ROLE, MicMode};
use omuzaki_audio_api::ids::{BusId, DeviceRole, NodeId};
use omuzaki_audio_api::node::{MAX_BLOCK, ParamIndex, SAMPLE_RATE, node_kind};
use omuzaki_audio_api::report::{
    AudioEvent, AudioEvents, AudioStatus, BridgeStatus, BusLatency, ClockSource, EventSender, GraphReport,
    LatencyEstimate, LeavingTap, MeterCell, MeterPoint, Meters, TapDrops, event_queue,
};
use omuzaki_audio_api::ring::ring;
use omuzaki_audio_api::semantics::db_to_gain;
use omuzaki_audio_api::tap::{
    DropPolicy, FeedRequest, FeedWriter, TapHandle, TapInfo, TapPoint, TapRate, TapReader, TapRequest, TapSink,
    VoiceRouting, VoiceSetup,
};
use omuzaki_core::time::Seconds;

use crate::dsp::{Decimator, Resampler};
use crate::executor::{
    Bridge, BridgeWriter, Command, Executor, Feed, Garbage, MAX_ENTRIES, Shared, Tap, TapLink, lock,
};
use crate::mix::Mix;

/// The callback size the engine asks devices for (design section 3.2).
pub const BUFFER_FRAMES: u32 = 256;
/// How much audio a bridge ring holds per channel: enough for a device that delivers in large bursts.
const BRIDGE_SECONDS: f64 = 0.25;
/// How often the housekeeper runs.
const HOUSEKEEPING: Duration = Duration::from_millis(5);
/// How often a lost device is tried again.
const RETRY: Duration = Duration::from_secs(2);
/// Commands that may wait for the audio thread.
const COMMAND_QUEUE: usize = 1024;
/// The role of the raw voice input when a voice changer has its own device (design section 4.2).
const RAW_VOICE_ROLE: &str = "raw_voice";

/// One open input.
struct Input {
    token: u64,
    binding: Binding,
    stream: Option<Box<dyn DeviceStream>>,
    failed: Arc<AtomicBool>,
    rate: u32,
    underruns: Arc<AtomicU64>,
    lost_at: Option<Instant>,
    retry_at: Option<Instant>,
}

/// A tap the control side remembers for the health meter.
struct TapRecord {
    consumer: String,
    point: TapPoint,
    leaves_device: bool,
    dropped: Arc<AtomicU64>,
}

/// The control side's state.
struct Control {
    graph: GraphSpec,
    generation: u64,
    /// Bus ids by their position in the running compiled graph, and each bus's chain node ids.
    order: Vec<(BusId, Vec<NodeId>)>,
    mic_mode: MicMode,
    bindings: BTreeMap<DeviceRole, Binding>,
    inputs: BTreeMap<DeviceRole, Input>,
    states: BTreeMap<DeviceRole, DeviceState>,
    bus_meters: BTreeMap<BusId, Arc<MeterCell>>,
    input_meters: BTreeMap<DeviceRole, Arc<MeterCell>>,
    taps: Vec<TapRecord>,
    entries: usize,
    changer_latency: Option<LatencyEstimate>,
}

struct Inner {
    backend: Arc<dyn AudioBackend>,
    executor: Arc<Mutex<Executor>>,
    commands: SyncSender<Command>,
    garbage: Mutex<Receiver<Garbage>>,
    clock: Arc<EngineClock>,
    shared: Arc<Shared>,
    control: Mutex<Control>,
    events: Mutex<Vec<EventSender>>,
    /// The token of the bridge whose callback runs the executor; 0 while the timer does.
    clock_token: Arc<AtomicU64>,
    next_token: AtomicU64,
    stop: AtomicBool,
    profile: Option<Arc<dyn ProfileStore>>,
    /// The pipeline clock's zero.
    epoch: Instant,
}

/// The live audio engine.
pub struct LiveEngine {
    inner: Arc<Inner>,
    housekeeper: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for LiveEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveEngine").finish_non_exhaustive()
    }
}

fn internal(e: impl std::fmt::Display) -> AudioError {
    AudioError::Internal(e.to_string())
}

impl LiveEngine {
    /// An engine on `backend`, with bindings kept in `profile` (or in memory when `None`), running the empty graph
    /// until [`AudioEngine::apply`] gives it one. Bindings already in the profile are opened.
    ///
    /// # Errors
    /// [`AudioError::Internal`] when the housekeeping thread cannot start; [`AudioError::Profile`] when the profile
    /// cannot be read.
    pub fn new(backend: Arc<dyn AudioBackend>, profile: Option<Arc<dyn ProfileStore>>) -> Result<Self, AudioError> {
        Self::with_epoch(backend, profile, Instant::now())
    }

    /// As [`new`](Self::new), with the pipeline clock's zero at `epoch`: the same instant the backend counts device
    /// times from (`CpalBackend::new`) and the stream service counts capture times from, so the engine clock, frames
    /// and takes share one timeline. The timer clock (no device) stamps its blocks from it too.
    ///
    /// # Errors
    /// As [`new`](Self::new).
    pub fn with_epoch(
        backend: Arc<dyn AudioBackend>,
        profile: Option<Arc<dyn ProfileStore>>,
        epoch: Instant,
    ) -> Result<Self, AudioError> {
        let bindings = match &profile {
            Some(p) => p.bindings()?,
            None => BTreeMap::new(),
        };
        let (commands, command_rx) = sync_channel(COMMAND_QUEUE);
        let (garbage_tx, garbage) = sync_channel(4 * MAX_ENTRIES);
        let clock = Arc::new(EngineClock::new(ClockSnapshot::nominal(SAMPLE_RATE)));
        let shared = Arc::new(Shared::default());
        let empty = Box::new(Mix::empty(0));
        let executor = Executor::new(empty, command_rx, garbage_tx, Arc::clone(&clock), Arc::clone(&shared));
        let inner = Arc::new(Inner {
            backend,
            executor: Arc::new(Mutex::new(executor)),
            commands,
            garbage: Mutex::new(garbage),
            clock,
            shared,
            control: Mutex::new(Control {
                graph: GraphSpec::default(),
                generation: 0,
                order: Vec::new(),
                mic_mode: MicMode::Live,
                bindings: BTreeMap::new(),
                inputs: BTreeMap::new(),
                states: BTreeMap::new(),
                bus_meters: BTreeMap::new(),
                input_meters: BTreeMap::new(),
                taps: Vec::new(),
                entries: 0,
                changer_latency: None,
            }),
            events: Mutex::new(Vec::new()),
            clock_token: Arc::new(AtomicU64::new(0)),
            next_token: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            profile,
            epoch,
        });
        let worker = Arc::clone(&inner);
        let housekeeper = std::thread::Builder::new()
            .name("omuzaki-audio-house".into())
            .spawn(move || housekeep(&worker))
            .map_err(internal)?;
        let engine = Self { inner, housekeeper: Mutex::new(Some(housekeeper)) };
        for (role, binding) in bindings {
            engine.inner.bind(&role, binding);
        }
        Ok(engine)
    }

    /// The engine's own timeline origin is its backend's; this is the frame count made so far.
    #[must_use]
    pub fn frames_made(&self) -> u64 {
        self.inner.shared.sample.load(Ordering::Relaxed)
    }
}

impl Drop for LiveEngine {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Release);
        if let Some(h) = lock(&self.housekeeper).take() {
            let _ = h.join();
        }
        // Closing the streams stops every callback before the executor goes.
        let mut control = lock(&self.inner.control);
        for input in control.inputs.values_mut() {
            drop(input.stream.take());
        }
    }
}

/// The housekeeper: frees garbage, drives the timer clock, watches streams.
fn housekeep(inner: &Inner) {
    let mut last = Instant::now();
    let mut carry = 0.0f64;
    let mut last_watch = Instant::now();
    while !inner.stop.load(Ordering::Acquire) {
        std::thread::sleep(HOUSEKEEPING);
        inner.drain_garbage();
        let now = Instant::now();
        let elapsed = now.duration_since(last).as_secs_f64();
        last = now;
        if inner.clock_token.load(Ordering::Acquire) == 0 {
            carry += elapsed * f64::from(SAMPLE_RATE);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A few hundred frames per tick.
            let frames = carry.floor() as usize;
            if frames > 0
                && let Ok(mut executor) = inner.executor.try_lock()
            {
                // The block's first frame is `carry` frames old: it should have been made that long ago.
                #[allow(clippy::cast_precision_loss)]
                let first = now.duration_since(inner.epoch).as_secs_f64() - carry / f64::from(SAMPLE_RATE);
                executor.run(frames, Some(Seconds(first)));
                #[allow(clippy::cast_precision_loss)]
                {
                    carry -= frames as f64;
                }
            }
        } else {
            carry = 0.0;
        }
        if now.duration_since(last_watch) >= Duration::from_millis(100) {
            last_watch = now;
            inner.watch(now);
        }
    }
}

impl Inner {
    fn send(&self, command: Command) -> Result<(), AudioError> {
        self.commands.try_send(command).map_err(|_| internal("the audio thread is not keeping up with commands"))
    }

    fn drain_garbage(&self) {
        let garbage = lock(&self.garbage);
        while let Ok(g) = garbage.try_recv() {
            drop(g);
        }
    }

    fn emit(&self, event: &AudioEvent) {
        let mut senders = lock(&self.events);
        senders.retain(EventSender::is_subscribed);
        for s in senders.iter() {
            s.send(event.clone());
        }
    }

    fn set_state(&self, control: &mut Control, role: &DeviceRole, state: DeviceState) {
        if control.states.get(role) != Some(&state) {
            control.states.insert(role.clone(), state.clone());
            self.emit(&AudioEvent::DeviceStateChanged { role: role.clone(), state });
        }
    }

    /// Marks failed streams lost and reopens lost ones on schedule.
    fn watch(&self, now: Instant) {
        let mut control = lock(&self.control);
        let roles: Vec<DeviceRole> = control.inputs.keys().cloned().collect();
        for role in roles {
            let Some(input) = control.inputs.get_mut(&role) else { continue };
            if input.failed.swap(false, Ordering::AcqRel) {
                drop(input.stream.take());
                input.lost_at.get_or_insert(now);
                input.retry_at = Some(now + RETRY);
                let token = input.token;
                if self.clock_token.compare_exchange(token, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    self.emit(&AudioEvent::ClockSourceChanged { source: ClockSource::Timer });
                }
                self.set_state(&mut control, &role, DeviceState::Lost);
            } else if input.stream.is_none() && input.retry_at.is_some_and(|t| now >= t) {
                let binding = input.binding.clone();
                let lost_at = input.lost_at;
                self.open_input(&mut control, &role, binding);
                if let (Some(input), Some(lost_at)) = (control.inputs.get(&role), lost_at)
                    && input.stream.is_some()
                {
                    self.emit(&AudioEvent::Reconnected {
                        role,
                        gap: Seconds(now.duration_since(lost_at).as_secs_f64()),
                    });
                }
            }
        }
    }

    fn resolve_device(&self, binding: &Binding, direction: Direction) -> Result<DeviceId, BackendError> {
        match binding {
            Binding::Fixed { id, .. } => Ok(id.clone()),
            Binding::SystemDefault { .. } => self
                .backend
                .default_device(direction)?
                .ok_or_else(|| BackendError::Other("the system has no default device".into())),
        }
    }

    /// Binds `role` (never failing: a device that cannot open is shown as `Failed` and retried).
    fn bind(&self, role: &DeviceRole, binding: Binding) {
        let mut control = lock(&self.control);
        control.bindings.insert(role.clone(), binding.clone());
        if role.as_str() == MONITOR_OUT_ROLE || role.as_str().ends_with("_out") {
            // Playback is not run by the spine engine: the binding is kept (it gates `CohostOnly`), nothing opens.
            self.set_state(&mut control, role, DeviceState::Failed { reason: "playback is not built yet".into() });
            return;
        }
        self.open_input(&mut control, role, binding);
    }

    fn close_input(&self, control: &mut Control, role: &DeviceRole) {
        if let Some(mut input) = control.inputs.remove(role) {
            drop(input.stream.take());
            let _ = self.clock_token.compare_exchange(input.token, 0, Ordering::AcqRel, Ordering::Acquire);
            let _ = self.send(Command::RemoveBridge(input.token));
            control.entries = control.entries.saturating_sub(1);
        }
        control.input_meters.remove(role);
    }

    fn open_input(&self, control: &mut Control, role: &DeviceRole, binding: Binding) {
        let lost_at = control.inputs.get(role).and_then(|i| i.lost_at);
        self.close_input(control, role);
        self.set_state(control, role, DeviceState::Opening);
        match self.try_open_input(control, role, &binding) {
            Ok(()) => self.set_state(control, role, DeviceState::Running),
            Err(e) => {
                let token = self.next_token.fetch_add(1, Ordering::Relaxed);
                control.inputs.insert(
                    role.clone(),
                    Input {
                        token,
                        binding,
                        stream: None,
                        failed: Arc::new(AtomicBool::new(false)),
                        rate: 0,
                        underruns: Arc::new(AtomicU64::new(0)),
                        lost_at,
                        retry_at: Some(Instant::now() + RETRY),
                    },
                );
                self.set_state(control, role, DeviceState::Failed { reason: e.to_string() });
            }
        }
    }

    fn try_open_input(&self, control: &mut Control, role: &DeviceRole, binding: &Binding) -> Result<(), AudioError> {
        if control.entries >= MAX_ENTRIES {
            return Err(internal("too many open inputs, taps and feeds"));
        }
        let id = self.resolve_device(binding, Direction::Input)?;
        let map: Vec<usize> = binding.channels().channels().iter().take(2).map(|c| usize::from(*c)).collect();
        let map = if map.is_empty() { vec![0] } else { map };
        let device_channels = map.iter().max().map_or(1, |m| m + 1);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A positive constant of a few thousand.
        let capacity = (BRIDGE_SECONDS * f64::from(SAMPLE_RATE)) as usize;
        let (producers, consumers): (Vec<_>, Vec<_>) =
            map.iter().map(|_| ring(capacity, DropPolicy::DropNewest, 0)).unzip();
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let started = Arc::new(AtomicBool::new(false));
        let bridge = Bridge::new(token, role.clone(), consumers, Arc::clone(&started));
        let meter = Arc::clone(&bridge.meter);
        let underruns = Arc::clone(&bridge.underruns);
        self.send(Command::AddBridge(Box::new(bridge)))?;
        // The device's real rate and channel count are known once the stream opens; until then the callback discards.
        let rate = Arc::new(AtomicU32::new(0));
        let channels = Arc::new(AtomicU32::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let mut writer = BridgeWriter {
            resamplers: map.iter().map(|_| Resampler::new(SAMPLE_RATE)).collect(),
            producers,
            map,
            device_channels: 0,
            scratch: Vec::with_capacity(8192),
            resampled: Vec::with_capacity(16_384),
            started,
        };
        let (cb_rate, cb_channels) = (Arc::clone(&rate), Arc::clone(&channels));
        let (executor, clock_token, shared) =
            (Arc::clone(&self.executor), Arc::clone(&self.clock_token), Arc::clone(&self.shared));
        let mut seen_rate = 0;
        let mut pending = 0usize;
        let on_data = Box::new(move |samples: &[f32], time: omuzaki_audio_api::backend::CallbackTime| {
            let (device_rate, device_channels) = (cb_rate.load(Ordering::Acquire), cb_channels.load(Ordering::Acquire));
            if device_rate == 0 || device_channels == 0 {
                return;
            }
            if device_rate != seen_rate {
                seen_rate = device_rate;
                for r in &mut writer.resamplers {
                    *r = Resampler::new(device_rate);
                }
            }
            writer.device_channels = device_channels as usize;
            pending += writer.write(samples);
            if clock_token.load(Ordering::Acquire) == token {
                if let Ok(mut ex) = executor.try_lock() {
                    ex.run(pending, Some(time.device));
                    pending = 0;
                } else {
                    shared.missed.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                pending = 0;
            }
        });
        let cb_failed = Arc::clone(&failed);
        let on_error = Box::new(move |e: BackendError| {
            if matches!(e, BackendError::DeviceNotAvailable | BackendError::DeviceChanged) {
                cb_failed.store(true, Ordering::Release);
            }
        });
        let wanted = StreamConfig {
            rate: SAMPLE_RATE,
            channels: u16::try_from(device_channels).unwrap_or(1),
            buffer_frames: BUFFER_FRAMES,
        };
        let stream = match self.backend.open_input(&id, wanted, on_data, on_error) {
            Ok(s) => s,
            Err(e) => {
                let _ = self.send(Command::RemoveBridge(token));
                return Err(e.into());
            }
        };
        let config = stream.config();
        channels.store(u32::from(config.channels), Ordering::Release);
        rate.store(config.rate, Ordering::Release);
        control.entries += 1;
        control.input_meters.insert(role.clone(), meter);
        let lost_at = None;
        control.inputs.insert(
            role.clone(),
            Input {
                token,
                binding: binding.clone(),
                stream: Some(stream),
                failed,
                rate: config.rate,
                underruns,
                lost_at,
                retry_at: None,
            },
        );
        if self.clock_token.compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            self.emit(&AudioEvent::ClockSourceChanged { source: ClockSource::Device });
        }
        Ok(())
    }
}

/// Where a requested tap reads, and the voice consumer when it was asked for by use.
fn resolve_tap(
    control: &Control,
    requested: &TapPoint,
) -> Result<(TapPoint, Option<omuzaki_audio_api::tap::VoiceUse>), AudioError> {
    let (point, voice) = match requested {
        TapPoint::Voice(consumer) => {
            let mic = BusId::new(MIC_BUS).map_err(internal)?;
            let raw_voice = DeviceRole::new(RAW_VOICE_ROLE).map_err(internal)?;
            let setup =
                if control.bindings.contains_key(&raw_voice) { VoiceSetup::TwoInputs } else { VoiceSetup::OwnChain };
            let routing = VoiceRouting { mic: &mic, raw_voice: &raw_voice };
            (control.graph.voice_taps.resolve(*consumer, setup, routing).point, Some(*consumer))
        }
        other => (other.clone(), None),
    };
    if let TapPoint::BusPreChain(bus) | TapPoint::BusPostChain(bus) | TapPoint::BusPostFader(bus) = &point
        && control.graph.bus(bus).is_none()
    {
        return Err(AudioError::TapUnavailable(format!("the graph has no bus `{bus}`")));
    }
    Ok((point, voice))
}

/// The declared latency, in 48 kHz samples, from the raw input to `point`.
fn point_latency(control: &Control, point: &TapPoint) -> u32 {
    let chain = |bus: &BusId| {
        control.graph.bus(bus).map_or(0, |b| {
            b.chain.iter().filter(|n| n.enabled).filter_map(|n| node_kind(&n.kind).map(|k| k.info().latency.max)).sum()
        })
    };
    match point {
        TapPoint::BusPostChain(bus) | TapPoint::BusPostFader(bus) => chain(bus),
        _ => 0,
    }
}

impl LiveEngine {
    fn open_tap(&self, request: &TapRequest, link: impl FnOnce(u64, usize) -> TapLink) -> Result<TapInfo, AudioError> {
        let inner = &self.inner;
        let mut control = lock(&inner.control);
        if control.entries >= MAX_ENTRIES {
            return Err(AudioError::TapUnavailable("too many open taps".into()));
        }
        let (point, voice) = resolve_tap(&control, &request.point)?;
        let speech = request.format.rate == TapRate::Speech16k;
        if speech && !request.format.mono {
            return Err(AudioError::TapUnavailable("16 kHz taps are mono".into()));
        }
        let stereo_point = match &point {
            TapPoint::BusPreChain(b) | TapPoint::BusPostChain(b) | TapPoint::BusPostFader(b) => {
                control.graph.bus(b).is_some_and(|b| b.channels.count() == 2)
            }
            TapPoint::Input(role) => control.bindings.get(role).is_some_and(|b| b.channels().channels().len() >= 2),
            TapPoint::Voice(_) => false,
        };
        let channels = if stereo_point && !request.format.mono && !speech { 2 } else { 1 };
        let rate = request.format.rate.hz();
        let clock_sample = inner.shared.sample.load(Ordering::Relaxed);
        let first_sample = if speech { clock_sample.div_ceil(3) } else { clock_sample };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Ring sizes: seconds of audio.
        let capacity = ((request.capacity.0.max(0.01) * f64::from(rate)) as usize).max(MAX_BLOCK) * channels;
        let dropped = Arc::new(AtomicU64::new(0));
        let mut latency = point_latency(&control, &point);
        if speech {
            latency += Decimator::delay();
        }
        let tap = Tap {
            point: point.clone(),
            voice,
            mono: channels == 1,
            speech,
            decimator: Decimator::new(),
            link: link(first_sample, capacity),
            flags: omuzaki_audio_api::ring::TapFlags::default(),
            scratch: Vec::with_capacity(2 * MAX_BLOCK),
            dropped: Arc::clone(&dropped),
        };
        inner.send(Command::AddTap(Box::new(tap)))?;
        control.entries += 1;
        control.taps.push(TapRecord {
            consumer: request.consumer.clone(),
            point,
            leaves_device: request.leaves_device,
            dropped,
        });
        Ok(TapInfo {
            first_sample,
            rate,
            capture_offset: Seconds(f64::from(latency) / f64::from(SAMPLE_RATE)),
            honours_mute: voice.is_some() || matches!(request.point, TapPoint::BusPostFader(_)),
        })
    }

    fn prune_records(control: &mut Control) {
        // A record whose tap the executor let go (and the housekeeper freed) holds the last reference to its counter.
        let before = control.taps.len();
        control.taps.retain(|t| Arc::strong_count(&t.dropped) > 1);
        control.entries = control.entries.saturating_sub(before - control.taps.len());
    }
}

impl AudioEngine for LiveEngine {
    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        Ok(self.inner.backend.devices()?)
    }

    fn bind(&self, role: &DeviceRole, binding: Binding) -> Result<(), AudioError> {
        if let Some(profile) = &self.inner.profile {
            profile.bind(role, binding.clone())?;
        }
        self.inner.bind(role, binding);
        Ok(())
    }

    fn unbind(&self, role: &DeviceRole) -> Result<(), AudioError> {
        if let Some(profile) = &self.inner.profile {
            profile.unbind(role)?;
        }
        let mut control = lock(&self.inner.control);
        control.bindings.remove(role);
        self.inner.close_input(&mut control, role);
        self.inner.set_state(&mut control, role, DeviceState::Unbound);
        control.states.remove(role);
        Ok(())
    }

    fn apply(&self, graph: &GraphSpec) -> Result<(), AudioError> {
        let mut control = lock(&self.inner.control);
        let generation = control.generation + 1;
        let (compiled, order) = Mix::compile(graph, generation, control.mic_mode, Some(&control.graph))?;
        let meters = compiled.buses.iter().map(|b| (b.id.clone(), Arc::clone(&b.meter))).collect();
        self.inner.send(Command::Swap(Box::new(compiled)))?;
        control.generation = generation;
        control.order = order;
        control.bus_meters = meters;
        control.graph = graph.clone();
        Ok(())
    }

    fn graph(&self) -> GraphSpec {
        lock(&self.inner.control).graph.clone()
    }

    fn set_param(&self, bus: &BusId, node: &NodeId, param: ParamIndex, value: f32) -> Result<(), AudioError> {
        let mut control = lock(&self.inner.control);
        let (at, nodes) = control
            .order
            .iter()
            .enumerate()
            .find(|(_, (id, _))| id == bus)
            .map(|(i, (_, nodes))| (i, nodes.clone()))
            .ok_or_else(|| AudioError::UnknownBus(bus.clone()))?;
        let node_at = nodes
            .iter()
            .position(|n| n == node)
            .ok_or_else(|| AudioError::UnknownNode { bus: bus.clone(), node: node.clone() })?;
        let spec = control
            .graph
            .buses
            .iter_mut()
            .find(|b| &b.id == bus)
            .and_then(|b| b.chain.iter_mut().find(|n| &n.id == node))
            .ok_or_else(|| AudioError::UnknownNode { bus: bus.clone(), node: node.clone() })?;
        let info = node_kind(&spec.kind)
            .map(omuzaki_audio_api::NodeKind::info)
            .ok_or_else(|| internal("a running node lost its kind"))?;
        let name = info
            .params
            .get(usize::from(param.0))
            .map(|p| p.name.to_owned())
            .ok_or_else(|| AudioError::UnknownParam { node: node.clone(), index: param.0 })?;
        spec.params.insert(name, value);
        let generation = control.generation;
        self.inner.send(Command::Param { generation, bus: at, node: node_at, param, value })
    }

    fn set_bus_gain_db(&self, bus: &BusId, db: f32) -> Result<(), AudioError> {
        let mut control = lock(&self.inner.control);
        let at =
            control.order.iter().position(|(id, _)| id == bus).ok_or_else(|| AudioError::UnknownBus(bus.clone()))?;
        if let Some(b) = control.graph.buses.iter_mut().find(|b| &b.id == bus) {
            b.gain_db = db;
        }
        let generation = control.generation;
        self.inner.send(Command::Gain { generation, bus: at, gain: db_to_gain(db) })
    }

    fn set_bus_muted(&self, bus: &BusId, muted: bool) -> Result<(), AudioError> {
        let mut control = lock(&self.inner.control);
        let at =
            control.order.iter().position(|(id, _)| id == bus).ok_or_else(|| AudioError::UnknownBus(bus.clone()))?;
        if let Some(b) = control.graph.buses.iter_mut().find(|b| &b.id == bus) {
            b.muted = muted;
        }
        let generation = control.generation;
        self.inner.send(Command::Mute { generation, bus: at, muted })
    }

    fn mic_mode(&self) -> MicMode {
        lock(&self.inner.control).mic_mode
    }

    fn set_mic_mode(&self, mode: MicMode) -> Result<MicMode, AudioError> {
        let mut control = lock(&self.inner.control);
        let landed = match (control.mic_mode, mode) {
            (MicMode::CohostOnly, MicMode::Live) => MicMode::Muted,
            (_, MicMode::CohostOnly) => {
                let monitor = DeviceRole::new(MONITOR_OUT_ROLE).map_err(internal)?;
                if !control.bindings.contains_key(&monitor) {
                    return Err(AudioError::NeedsMonitorOutput);
                }
                MicMode::CohostOnly
            }
            (_, other) => other,
        };
        self.inner.send(Command::MicMode(landed))?;
        let changed = control.mic_mode != landed;
        control.mic_mode = landed;
        drop(control);
        if changed {
            self.inner.emit(&AudioEvent::MicModeChanged { mode: landed });
        }
        Ok(landed)
    }

    fn tap(&self, request: &TapRequest, sink: Box<dyn TapSink>) -> Result<(TapHandle, TapInfo), AudioError> {
        let (handle, closed) = TapHandle::new();
        let info = self.open_tap(request, |_, _| TapLink::Sink(sink, closed))?;
        Ok((handle, info))
    }

    fn tap_ring(&self, request: &TapRequest) -> Result<(TapReader, TapInfo), AudioError> {
        let mut reader = None;
        let info = self.open_tap(request, |first, capacity| {
            let (producer, consumer) = ring(capacity, request.policy, first);
            reader = Some(consumer);
            TapLink::Ring(producer)
        })?;
        let reader = reader.ok_or_else(|| internal("the tap ring was not made"))?;
        Ok((reader, info))
    }

    fn feed(&self, request: &FeedRequest) -> Result<FeedWriter, AudioError> {
        let mut control = lock(&self.inner.control);
        if control.entries >= MAX_ENTRIES {
            return Err(internal("too many open feeds"));
        }
        if request.rate == 0 {
            return Err(internal("a feed needs a rate above zero"));
        }
        let channels = if request.mono { 1 } else { 2 };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Ring sizes: seconds of audio.
        let capacity = ((request.capacity.0.max(0.05) * f64::from(request.rate)) as usize).max(MAX_BLOCK) * channels;
        let (producer, consumer) = ring(capacity, DropPolicy::DropNewest, 0);
        self.inner.send(Command::AddFeed(Box::new(Feed::new(
            request.id.clone(),
            consumer,
            !request.mono,
            request.rate,
        ))))?;
        control.entries += 1;
        Ok(producer)
    }

    fn clock(&self) -> Arc<EngineClock> {
        Arc::clone(&self.inner.clock)
    }

    fn status(&self) -> AudioStatus {
        let mut control = lock(&self.inner.control);
        Self::prune_records(&mut control);
        let token = self.inner.clock_token.load(Ordering::Acquire);
        let clock_role = control.inputs.iter().find(|(_, i)| i.token == token && token != 0).map(|(r, _)| r.clone());
        let bridges = control
            .inputs
            .iter()
            .filter(|(_, i)| i.token != token)
            .map(|(role, i)| BridgeStatus {
                role: role.clone(),
                state: control.states.get(role).cloned().unwrap_or(DeviceState::Unbound),
                resampled_from: (i.rate != SAMPLE_RATE && i.rate != 0).then_some(i.rate),
                buffering: Seconds(0.0),
                drift_ppm: 0.0,
                underruns: i.underruns.load(Ordering::Relaxed),
                overruns: 0,
            })
            .collect();
        AudioStatus {
            clock_source: if clock_role.is_some() { ClockSource::Device } else { ClockSource::Timer },
            clock_role,
            block_frames: self.inner.shared.block_frames.load(Ordering::Relaxed),
            devices: control.states.iter().map(|(r, s)| (r.clone(), s.clone())).collect(),
            bridges,
            mic_mode: control.mic_mode,
            aec_on: false,
            tap_drops: control
                .taps
                .iter()
                .map(|t| TapDrops { consumer: t.consumer.clone(), dropped: t.dropped.load(Ordering::Relaxed) })
                .collect(),
            leaving_device: control
                .taps
                .iter()
                .filter(|t| t.leaves_device)
                .map(|t| LeavingTap { consumer: t.consumer.clone(), point: t.point.clone() })
                .collect(),
            model_blocks_missed: 0,
        }
    }

    fn report(&self) -> GraphReport {
        let control = lock(&self.inner.control);
        let buses = control
            .graph
            .buses
            .iter()
            .map(|b| BusLatency {
                bus: b.id.clone(),
                chain_samples: point_latency(&control, &TapPoint::BusPostChain(b.id.clone())),
            })
            .collect();
        GraphReport { routes: Vec::new(), buses }
    }

    fn meters(&self, at: &MeterPoint) -> Option<Meters> {
        let control = lock(&self.inner.control);
        match at {
            MeterPoint::Bus(id) => control.bus_meters.get(id).map(|m| m.load()),
            MeterPoint::Input(role) => control.input_meters.get(role).map(|m| m.load()),
        }
    }

    fn set_changer_latency(&self, measured: Option<LatencyEstimate>) -> Result<(), AudioError> {
        lock(&self.inner.control).changer_latency = measured;
        Ok(())
    }

    fn subscribe(&self) -> AudioEvents {
        let (sender, events) = event_queue();
        lock(&self.inner.events).push(sender);
        events
    }
}
