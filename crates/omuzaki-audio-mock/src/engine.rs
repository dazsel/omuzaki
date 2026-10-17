//! [`MockEngine`]: an [`AudioEngine`] for tests of the ops and consumers that use one.
//!
//! It keeps no thread and no clock of its own. The graph it was given is run by the reference mixer, one block at a
//! time, when a test calls [`MockEngine::advance`]; taps and feeds are the real SPSC rings of `omuzaki-audio-api`.
//! Everything is deterministic.
//!
//! What it simplifies, on purpose:
//! - the devices are a fixed list ([`mock_devices`]) and every bound role is `Running`; there are no bridges, no loss
//!   and no clock device (the clock source is always the timer);
//! - device inputs are silent: audio enters only through feeds (a `SourceRef::Feed` source);
//! - `apply` rebuilds the mixer, so node state and ramps start fresh, and the live setters are applied to the running
//!   mixer as ramps;
//! - a feed is 48 kHz only (no resampling at the edge), and a closed feed goes silent from the next block with no fade;
//! - a tap that is not mixed down is refused for a stereo point (one scalar ring cannot carry two channels with
//!   frame-true positions);
//! - meters are the peak and RMS of the **last block** (not 300 ms), and the program-only fields keep their neutral
//!   values;
//! - `LatencyChanged` events are not sent.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_audio_api::backend::BackendError;
use omuzaki_audio_api::clock::{ClockSnapshot, EngineClock};
use omuzaki_audio_api::device::{Binding, DeviceId, DeviceInfo, DeviceState, Direction};
use omuzaki_audio_api::engine::{AudioEngine, AudioError, Planar};
use omuzaki_audio_api::graph::{AecMode, Channels, GraphSpec, MONITOR_OUT_ROLE, MicMode, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId, NodeId};
use omuzaki_audio_api::node::{MAX_BLOCK, NodeKind, ParamIndex, SAMPLE_RATE, node_kind};
use omuzaki_audio_api::report::{
    AudioEvent, AudioEvents, AudioStatus, ClockSource, EventSender, GraphReport, LatencyEstimate, LeavingTap,
    MeterPoint, Meters, RouteEnd, TapDrops, event_queue,
};
use omuzaki_audio_api::ring::{Consumer, Producer, TapFlags, ring};
use omuzaki_audio_api::tap::{
    DropPolicy, FeedRequest, FeedWriter, TapClosed, TapHandle, TapInfo, TapPoint, TapRate, TapReader, TapRequest,
    TapSink,
};
use omuzaki_core::time::Seconds;

use crate::mixer::{Mixer, check_gain};
use crate::taps::{TapPlan, TapProcessor, first_sample};

/// The block size `status()` reports before the first `advance`: the requested device buffer of design section 3.2.
const DEFAULT_BLOCK: u32 = 256;

/// The fixed device list of the mock engine: a microphone and a line input, speakers and headphones.
#[must_use]
pub fn mock_devices() -> Vec<DeviceInfo> {
    let device = |id: &str, label: &str, direction, is_default, channels| DeviceInfo {
        id: DeviceId { host: "mock".into(), id: id.into() },
        label: label.into(),
        direction,
        is_default,
        rates: vec![44_100, SAMPLE_RATE],
        channels,
        latency_hint: Some(Seconds(0.005)),
    };
    vec![
        device("mic-1", "Mock Microphone", Direction::Input, true, 1),
        device("line-1", "Mock Line In", Direction::Input, false, 2),
        device("out-1", "Mock Speakers", Direction::Output, true, 2),
        device("out-2", "Mock Headphones", Direction::Output, false, 2),
    ]
}

/// The end a tap's samples go to.
enum Link {
    /// The ring `tap_ring` made: closed when its reader is dropped.
    Ring(Producer),
    /// A consumer's sink from `tap`: closed when its handle is dropped. The count is the samples it refused.
    Sink(Box<dyn TapSink>, TapClosed, u64),
}

impl Link {
    fn is_closed(&self) -> bool {
        match self {
            Self::Ring(producer) => producer.reader_gone(),
            Self::Sink(_, closed, _) => closed.is_closed(),
        }
    }

    fn flags(&mut self, flags: TapFlags) {
        match self {
            Self::Ring(producer) => producer.set_flags(flags),
            Self::Sink(sink, ..) => sink.flags(flags),
        }
    }

    /// Hands `samples` over. A sink that accepts fewer than it was given has the rest counted as dropped.
    fn push(&mut self, samples: &[f32]) {
        match self {
            Self::Ring(producer) => {
                producer.push(samples);
            }
            Self::Sink(sink, _, refused) => {
                let accepted = sink.push(samples).min(samples.len());
                *refused += (samples.len() - accepted) as u64;
            }
        }
    }

    /// Samples lost so far: refused by a `DropNewest` ring or a sink, or overwritten in an `Overwrite` ring before its
    /// reader got to them.
    fn dropped(&self) -> u64 {
        match self {
            Self::Ring(producer) => producer.dropped() + producer.overwritten(),
            Self::Sink(_, _, refused) => *refused,
        }
    }
}

struct OpenTap {
    request: TapRequest,
    processor: TapProcessor,
    link: Link,
    flags: Option<TapFlags>,
    /// The offset `TapInfo` handed out. `TapInfo` is immutable, so a tap whose offset would change is closed instead.
    capture_offset: Seconds,
}

/// A feed: the reading end of its ring, and how many samples it has been short of.
struct OpenFeed {
    reader: Consumer,
    channels: usize,
    underrun_samples: u64,
}

struct State {
    devices: Vec<DeviceInfo>,
    bindings: BTreeMap<DeviceRole, Binding>,
    graph: GraphSpec,
    mixer: Mixer,
    mic_mode: MicMode,
    taps: Vec<OpenTap>,
    feeds: BTreeMap<FeedId, OpenFeed>,
    /// The last block's levels per bus.
    meters: BTreeMap<BusId, Meters>,
    subscribers: Vec<EventSender>,
    clock: Arc<EngineClock>,
    changer_latency: Option<LatencyEstimate>,
    last_block: u32,
    sample: u64,
}

/// The mock [`AudioEngine`].
pub struct MockEngine {
    state: Mutex<State>,
}

impl Default for MockEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn unknown_bus(graph: &GraphSpec, bus: &BusId) -> Result<(), AudioError> {
    graph.bus(bus).map(|_| ()).ok_or_else(|| AudioError::UnknownBus(bus.clone()))
}

/// The peak and RMS of a block in dBFS; the program-only fields keep their neutral values.
fn levels(block: &Planar) -> Meters {
    let samples: Vec<f32> = block.iter().flatten().copied().collect();
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    #[allow(clippy::cast_precision_loss)] // a block is at most 2,048 samples
    let mean_square = samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32;
    Meters { peak_db: 20.0 * peak.log10(), rms_db: 10.0 * mean_square.log10(), ..Meters::SILENT }
}

impl MockEngine {
    /// An engine with the fixed devices, an empty graph, the mic `Live` and the clock at sample 0.
    #[must_use]
    pub fn new() -> Self {
        Self::with_devices(mock_devices())
    }

    /// An engine whose `devices()` answers `devices`.
    #[must_use]
    pub fn with_devices(devices: Vec<DeviceInfo>) -> Self {
        let graph = GraphSpec::default();
        let mixer = Mixer::empty(MicMode::Live);
        let state = State {
            devices,
            bindings: BTreeMap::new(),
            graph,
            mixer,
            mic_mode: MicMode::Live,
            taps: Vec::new(),
            feeds: BTreeMap::new(),
            meters: BTreeMap::new(),
            subscribers: Vec::new(),
            clock: Arc::new(EngineClock::new(ClockSnapshot::nominal(SAMPLE_RATE))),
            changer_latency: None,
            last_block: DEFAULT_BLOCK,
            sample: 0,
        };
        Self { state: Mutex::new(state) }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A panic in another test thread cannot leave this state half-written in a way a test could rely on.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs the graph for `frames` frames, in blocks of at most `MAX_BLOCK`: feeds are read (a short feed plays
    /// silence for the rest), the graph is evaluated, and every open tap gets its samples. Taps whose reader or handle
    /// is gone are closed, and so are feeds whose writer is gone.
    pub fn advance(&self, frames: usize) {
        let mut state = self.state();
        let mut left = frames;
        while left > 0 {
            let block = left.min(MAX_BLOCK);
            state.block(block);
            left -= block;
        }
    }

    /// How many feed samples were missing when the graph wanted them (an underrun plays silence and is counted), for
    /// the feed `id`; `None` for a feed that is not open.
    #[must_use]
    pub fn feed_underruns(&self, id: &FeedId) -> Option<u64> {
        self.state().feeds.get(id).map(|f| f.underrun_samples)
    }

    /// The changer latency last stored with `set_changer_latency`.
    #[must_use]
    pub fn changer_latency(&self) -> Option<LatencyEstimate> {
        self.state().changer_latency.clone()
    }
}

impl State {
    fn emit(&mut self, event: &AudioEvent) {
        self.subscribers.retain(EventSender::is_subscribed);
        for subscriber in &self.subscribers {
            subscriber.send(event.clone());
        }
    }

    fn block(&mut self, frames: usize) {
        self.feeds.retain(|_, feed| !feed.reader.writer_gone());
        let sources = self.read_feeds(frames);
        self.taps.retain(|tap| !tap.link.is_closed());
        let want: BTreeSet<_> = self.taps.iter().map(|t| t.processor.plan().point.clone()).collect();
        let capture = self.mixer.process(&sources, 0, frames, &want);
        for (bus, block) in &capture.buses {
            self.meters.insert(bus.clone(), levels(block));
        }
        for tap in &mut self.taps {
            let Some(raw) = capture.points.get(&tap.processor.plan().point) else { continue };
            let flags = tap.processor.flags(self.mic_mode);
            if tap.flags != Some(flags) {
                tap.link.flags(flags);
                tap.flags = Some(flags);
            }
            let mut samples = Vec::new();
            tap.processor.process(raw, self.mic_mode, &mut samples);
            tap.link.push(&samples);
        }
        self.sample += frames as u64;
        self.last_block = u32::try_from(frames).unwrap_or(DEFAULT_BLOCK);
        self.publish_clock();
    }

    /// One block from every open feed, planar; a short feed is padded with silence and counted.
    fn read_feeds(&mut self, frames: usize) -> BTreeMap<SourceRef, Planar> {
        let mut sources = BTreeMap::new();
        for (id, feed) in &mut self.feeds {
            let wanted = frames * feed.channels;
            let mut interleaved = vec![0.0; wanted];
            let got = feed.reader.pop_into(&mut interleaved);
            feed.underrun_samples += (wanted - got) as u64;
            let planar: Planar =
                (0..feed.channels).map(|c| (0..frames).map(|n| interleaved[n * feed.channels + c]).collect()).collect();
            sources.insert(SourceRef::Feed { id: id.clone() }, planar);
        }
        sources
    }

    fn publish_clock(&self) {
        let rate = f64::from(SAMPLE_RATE);
        #[allow(clippy::cast_precision_loss)] // sample indices stay far below 2^53
        let time = Seconds(self.sample as f64 / rate);
        self.clock.publish(ClockSnapshot { sample: self.sample, time, seconds_per_sample: 1.0 / rate });
    }

    /// The latency of the route that ends at `plan`'s point, the smallest one, plus the 16 kHz decimator's delay.
    fn capture_offset(&self, plan: &TapPlan, rate: TapRate) -> Seconds {
        let report = self.mixer.report(std::slice::from_ref(&plan.point));
        let samples = report
            .routes
            .iter()
            .filter(|r| matches!(&r.to, RouteEnd::Tap { point } if *point == plan.point))
            .map(|r| r.graph_samples)
            .min()
            .unwrap_or(0);
        // The mean of three samples is centred one sample later than the first of them.
        let decimator = if rate == TapRate::Speech16k { 1.0 } else { 0.0 };
        Seconds((f64::from(samples) + decimator) / f64::from(SAMPLE_RATE))
    }

    fn open_tap(&mut self, request: &TapRequest, link: Link) -> Result<TapInfo, AudioError> {
        let plan = TapPlan::resolve(&self.graph, &request.point)?;
        check_tap_format(&self.graph, &plan, request)?;
        let info = TapInfo {
            first_sample: first_sample(request.format.rate, self.sample),
            rate: request.format.rate.hz(),
            capture_offset: self.capture_offset(&plan, request.format.rate),
            honours_mute: plan.honours_mute(),
        };
        let processor = TapProcessor::new(plan, request.format, self.sample);
        self.taps.push(OpenTap {
            request: request.clone(),
            processor,
            link,
            flags: None,
            capture_offset: info.capture_offset,
        });
        Ok(info)
    }
}

/// Whether `request`'s format can be served at `plan`'s point in `graph`. Checked when a tap opens and again whenever
/// `apply` re-resolves it, because a bus that becomes stereo changes the answer.
fn check_tap_format(graph: &GraphSpec, plan: &TapPlan, request: &TapRequest) -> Result<(), AudioError> {
    let stereo_point = matches!(
        &plan.point,
        TapPoint::BusPreChain(bus) | TapPoint::BusPostChain(bus) | TapPoint::BusPostFader(bus)
            if graph.bus(bus).is_some_and(|b| b.channels == Channels::Stereo)
    );
    if stereo_point && !request.format.mono && request.format.rate == TapRate::Graph48k {
        // The ring is a scalar stream, and the contract's positions count scalars: two channels in one ring
        // would stamp every run twice as late as the frame clock. Down-mix with `mono: true` instead.
        return Err(AudioError::TapUnavailable(format!(
            "a stereo point cannot be read as two channels through one ring ({:?}): ask for mono",
            plan.point
        )));
    }
    Ok(())
}

/// The largest ring the mock makes, in samples: about 87 s of mono 48 kHz audio, far above any default capacity.
const MAX_RING_SAMPLES: u32 = 1 << 22;

/// Samples of a ring that holds `seconds` of a signal at `rate`, at least one.
///
/// # Errors
/// [`AudioError::Internal`] when `seconds` is negative, not finite, or asks for more than [`MAX_RING_SAMPLES`].
fn ring_samples(seconds: Seconds, rate: u32) -> Result<usize, AudioError> {
    // Refused before rounding: `round` takes a tiny negative to negative zero, which the range check below accepts.
    let wanted = if seconds.0 < 0.0 { f64::NAN } else { (seconds.0 * f64::from(rate)).round() };
    if !(0.0..=f64::from(MAX_RING_SAMPLES)).contains(&wanted) {
        return Err(AudioError::Internal(format!(
            "a ring of {} s at {rate} Hz is outside 0 to {MAX_RING_SAMPLES} samples",
            seconds.0
        )));
    }
    // Checked to be within `0..=MAX_RING_SAMPLES` just above, so the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        Ok((wanted as usize).max(1))
    }
}

impl AudioEngine for MockEngine {
    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        Ok(self.state().devices.clone())
    }

    fn bind(&self, role: &DeviceRole, binding: Binding) -> Result<(), AudioError> {
        let mut state = self.state();
        if let Binding::Fixed { id, .. } = &binding
            && !state.devices.iter().any(|d| &d.id == id)
        {
            return Err(BackendError::UnknownDevice(id.clone()).into());
        }
        state.bindings.insert(role.clone(), binding);
        state.emit(&AudioEvent::DeviceStateChanged { role: role.clone(), state: DeviceState::Running });
        Ok(())
    }

    fn unbind(&self, role: &DeviceRole) -> Result<(), AudioError> {
        let mut state = self.state();
        if state.bindings.remove(role).is_some() {
            state.emit(&AudioEvent::DeviceStateChanged { role: role.clone(), state: DeviceState::Unbound });
        }
        Ok(())
    }

    fn apply(&self, graph: &GraphSpec) -> Result<(), AudioError> {
        let mut state = self.state();
        let mut mixer = Mixer::new(graph, state.mic_mode)?;
        mixer.next_sample = state.sample;
        state.mixer = mixer;
        state.graph = graph.clone();
        // A tap whose point the new graph no longer has is closed; one whose voice signal now resolves elsewhere
        // (`voice_taps` changed) follows it. So is one whose format the point can no longer serve (a bus that became
        // stereo), or whose `capture_offset` moved, because the offset in its `TapInfo` cannot be changed.
        let mut taps = std::mem::take(&mut state.taps);
        taps.retain_mut(|tap| {
            let Ok(plan) = TapPlan::resolve(&state.graph, &tap.request.point) else { return false };
            if check_tap_format(&state.graph, &plan, &tap.request).is_err() {
                return false;
            }
            if state.capture_offset(&plan, tap.request.format.rate) != tap.capture_offset {
                return false;
            }
            tap.processor.replan(plan);
            true
        });
        state.taps = taps;
        Ok(())
    }

    fn graph(&self) -> GraphSpec {
        self.state().graph.clone()
    }

    fn set_param(&self, bus: &BusId, node: &NodeId, param: ParamIndex, value: f32) -> Result<(), AudioError> {
        let mut state = self.state();
        let spec = state.graph.bus(bus).ok_or_else(|| AudioError::UnknownBus(bus.clone()))?;
        let node_spec = spec
            .chain
            .iter()
            .find(|n| &n.id == node)
            .ok_or_else(|| AudioError::UnknownNode { bus: bus.clone(), node: node.clone() })?;
        let info = node_kind(&node_spec.kind).map(NodeKind::info);
        let name = info
            .and_then(|i| i.params.get(usize::from(param.0)).map(|p| p.name))
            .ok_or_else(|| AudioError::UnknownParam { node: node.clone(), index: param.0 })?;
        state.mixer.set_param(bus, node, param, value)?;
        for spec in state.graph.buses.iter_mut().filter(|b| &b.id == bus) {
            for node_spec in spec.chain.iter_mut().filter(|n| &n.id == node) {
                node_spec.params.insert(name.to_string(), value);
            }
        }
        Ok(())
    }

    fn set_bus_gain_db(&self, bus: &BusId, db: f32) -> Result<(), AudioError> {
        let mut state = self.state();
        unknown_bus(&state.graph, bus)?;
        check_gain(bus, db)?;
        state.mixer.set_bus_gain_db(bus, db)?;
        state.graph.buses.iter_mut().filter(|b| &b.id == bus).for_each(|b| b.gain_db = db);
        Ok(())
    }

    fn set_bus_muted(&self, bus: &BusId, muted: bool) -> Result<(), AudioError> {
        let mut state = self.state();
        unknown_bus(&state.graph, bus)?;
        state.mixer.set_bus_muted(bus, muted)?;
        state.graph.buses.iter_mut().filter(|b| &b.id == bus).for_each(|b| b.muted = muted);
        Ok(())
    }

    fn mic_mode(&self) -> MicMode {
        self.state().mic_mode
    }

    fn set_mic_mode(&self, mode: MicMode) -> Result<MicMode, AudioError> {
        let mut state = self.state();
        let current = state.mic_mode;
        // Leaving `CohostOnly` lands on `Muted`, even when `Live` was asked for.
        let landed =
            if current == MicMode::CohostOnly && mode == MicMode::Live { current.after_leaving_cohost() } else { mode };
        if landed == MicMode::CohostOnly && current != MicMode::CohostOnly {
            let has_monitor = state.bindings.keys().any(|r| r.as_str() == MONITOR_OUT_ROLE);
            if !has_monitor {
                return Err(AudioError::NeedsMonitorOutput);
            }
        }
        if landed != current {
            state.mic_mode = landed;
            state.mixer.set_mic_mode(landed);
            state.emit(&AudioEvent::MicModeChanged { mode: landed });
        }
        Ok(landed)
    }

    fn tap(&self, request: &TapRequest, sink: Box<dyn TapSink>) -> Result<(TapHandle, TapInfo), AudioError> {
        let (handle, closed) = TapHandle::new();
        let info = self.state().open_tap(request, Link::Sink(sink, closed, 0))?;
        Ok((handle, info))
    }

    fn tap_ring(&self, request: &TapRequest) -> Result<(TapReader, TapInfo), AudioError> {
        let mut state = self.state();
        let capacity = ring_samples(request.capacity, request.format.rate.hz())?;
        let (producer, reader) = ring(capacity, request.policy, first_sample(request.format.rate, state.sample));
        let info = state.open_tap(request, Link::Ring(producer))?;
        Ok((reader, info))
    }

    fn feed(&self, request: &FeedRequest) -> Result<FeedWriter, AudioError> {
        if request.rate != SAMPLE_RATE {
            return Err(AudioError::Internal(format!(
                "the mock engine takes feeds at {SAMPLE_RATE} Hz only, not {} Hz",
                request.rate
            )));
        }
        let channels: usize = if request.mono { 1 } else { 2 };
        let capacity = ring_samples(request.capacity, request.rate)?.saturating_mul(channels);
        let (writer, reader) = ring(capacity, DropPolicy::DropNewest, 0);
        self.state().feeds.insert(request.id.clone(), OpenFeed { reader, channels, underrun_samples: 0 });
        Ok(writer)
    }

    fn clock(&self) -> Arc<EngineClock> {
        Arc::clone(&self.state().clock)
    }

    fn status(&self) -> AudioStatus {
        let state = self.state();
        let aec_on = match state.graph.aec {
            AecMode::On => true,
            AecMode::Off => false,
            AecMode::Auto => state.bindings.values().any(Binding::speakers),
        };
        AudioStatus {
            clock_source: ClockSource::Timer,
            clock_role: None,
            block_frames: state.last_block,
            devices: state.bindings.keys().map(|role| (role.clone(), DeviceState::Running)).collect(),
            bridges: Vec::new(),
            mic_mode: state.mic_mode,
            aec_on,
            tap_drops: state
                .taps
                .iter()
                .map(|t| TapDrops { consumer: t.request.consumer.clone(), dropped: t.link.dropped() })
                .collect(),
            leaving_device: state
                .taps
                .iter()
                .filter(|t| t.request.leaves_device)
                .map(|t| LeavingTap { consumer: t.request.consumer.clone(), point: t.request.point.clone() })
                .collect(),
            model_blocks_missed: 0,
        }
    }

    fn report(&self) -> GraphReport {
        let state = self.state();
        let points: Vec<_> = state.taps.iter().map(|t| t.processor.plan().point.clone()).collect();
        state.mixer.report(&points)
    }

    fn meters(&self, at: &MeterPoint) -> Option<Meters> {
        let state = self.state();
        match at {
            MeterPoint::Bus(bus) => {
                state.graph.bus(bus).map(|_| state.meters.get(bus).copied().unwrap_or(Meters::SILENT))
            }
            // Device inputs are silent in the mock, so a role the graph reads reads as silence.
            MeterPoint::Input(role) => state
                .graph
                .buses
                .iter()
                .any(|b| b.sources.iter().any(|s| matches!(s, SourceRef::Device { role: r } if r == role)))
                .then_some(Meters::SILENT),
        }
    }

    fn set_changer_latency(&self, measured: Option<LatencyEstimate>) -> Result<(), AudioError> {
        self.state().changer_latency = measured;
        Ok(())
    }

    fn subscribe(&self) -> AudioEvents {
        let (sender, events) = event_queue();
        self.state().subscribers.push(sender);
        events
    }
}
