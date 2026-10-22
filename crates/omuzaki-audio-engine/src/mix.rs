//! The mixer core: the compiled graph and its per-block signal flow, shared by the live executor and the offline render.
//!
//! [`Mix::compile`] runs on the control thread and allocates everything a block can need: the buses' buffers, each
//! node's hold-over delay, the ducking rules' level histories, the sidechain references and the output buffers.
//! [`Mix::process`] then runs on the audio thread and allocates, frees, locks and logs nothing.
//!
//! Per block, for each bus in topological order (a bus after every bus that sends to it), as design section 3.6 and the
//! reference mixer in `omuzaki-audio-mock` say:
//! 1. `pre` = what the earlier buses sent + the bus's sources, channel-converted to the bus (mono to stereo puts
//!    `x / sqrt(2)` on each side, stereo to mono is `(L + R) / sqrt(2)`);
//! 2. the chain: an enabled node processes the block; a disabled one only delays it by its latency, so switching a
//!    node off never shifts the bus in time; a node with a sidechain reads its bus's previous block;
//! 3. per sample: `post = pre × gain × mute × duck`, each factor its own ramp or envelope; pre-fader sends carry
//!    `pre × mute`, post-fader sends `post`, each through its own ramp; the outputs add `post` (cut on every output but
//!    `monitor_out` while `CohostOnly` keeps `ai_voice` private); a bus that triggers a duck rule feeds its level
//!    follower, which the rule reads `TRIGGER_DELAY` samples later, so the result never depends on the block plan.
//!
//! The recurrences (fader ramps, the level follower, the duck envelope, `db_to_gain`) are the ones in
//! `omuzaki_audio_api::semantics`, which both engines share; the property test in `tests/offline.rs` holds the two to
//! 1e-6 on random graphs and block plans.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use omuzaki_audio_api::engine::AudioError;
use omuzaki_audio_api::graph::{
    AI_VOICE_BUS, BusSpec, DuckTrigger, GraphSpec, MIC_BUS, MONITOR_OUT_ROLE, MicMode, PROGRAM_BUS, SourceRef,
};
use omuzaki_audio_api::ids::{BusId, DeviceRole, NodeId};
use omuzaki_audio_api::node::{BlockMut, MAX_BLOCK, Node, NodeError, node_kind};
use omuzaki_audio_api::report::MeterCell;
use omuzaki_audio_api::semantics::{
    DuckEnvelope, FaderRamp, LevelFollower, MONO_TO_STEREO_GAIN, TRIGGER_DELAY, db_to_gain, duck_factor,
};

use crate::dsp::HoldDelay;
use crate::meters::Meter;

/// Entries in a trigger's level history: twice the trigger delay, so a block of at most `MAX_BLOCK` samples never
/// overwrites a level a rule still has to read, whatever order the buses run in.
const HISTORY: usize = 2 * TRIGGER_DELAY;

/// Each bus's node ids in chain order, by bus position: how the control side names a node to the executor.
pub(crate) type NodeIds = Vec<(BusId, Vec<NodeId>)>;

/// One node in a chain.
pub(crate) struct Link {
    pub node: Box<dyn Node>,
    pub enabled: bool,
    /// Runs instead of the node while it is disabled: the dry signal, `node.latency()` samples late.
    hold: HoldDelay,
    /// The position of the bus whose previous block the node reads, for a kind with a sidechain.
    sidechain: Option<usize>,
}

/// One send, by its target's position.
pub(crate) struct Send {
    pub to: usize,
    gain: f32,
    pre_fader: bool,
    ramp: FaderRamp,
    /// The send leaves `ai_voice` for a bus that reaches an output other than `monitor_out`, so `CohostOnly` cuts it.
    private_cut: bool,
}

/// One compiled bus.
pub(crate) struct Bus {
    pub id: BusId,
    pub channels: usize,
    pub sources: Vec<SourceRef>,
    pub chain: Vec<Link>,
    is_mic: bool,
    is_ai_voice: bool,
    gain: FaderRamp,
    mute: FaderRamp,
    /// Applied to the outputs other than `monitor_out`: 0 for `ai_voice` while `CohostOnly` keeps its reply private.
    private_out: FaderRamp,
    muted: bool,
    pub sends: Vec<Send>,
    /// Positions in [`Mix::outputs`].
    outputs: Vec<usize>,
    /// Positions in [`Mix::rules`] of the rules that target this bus, in `GraphSpec::ducks` order.
    rules: Vec<usize>,
    /// The bus's input, then (after the chain) its pre-fader signal.
    pub buf: [Vec<f32>; 2],
    /// The bus after its fader.
    pub post: [Vec<f32>; 2],
    pub meter: Arc<MeterCell>,
    meter_state: Meter,
}

/// What raises a rule.
enum RuleTrigger {
    /// A bus's level history, compared with a threshold gain.
    Bus { bus: usize, threshold: f32 },
    /// A gate: opened by the player (#1160); closed until then.
    Gate,
}

struct Rule {
    envelope: DuckEnvelope,
    trigger: RuleTrigger,
}

/// A bus that triggers a rule: its level follower and the last `HISTORY` levels, indexed by sample number.
struct Trigger {
    follower: LevelFollower,
    levels: Vec<f32>,
}

/// What an output role would play: the sum of the buses that name it.
pub(crate) struct Output {
    pub role: DeviceRole,
    pub channels: usize,
    pub buf: [Vec<f32>; 2],
}

/// Where a tap can read in a bus.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    PreChain,
    PostChain,
    PostFader,
}

/// Where a bus's sources come from: the bridges and feeds live, the input's planar audio offline.
pub(crate) trait Inputs {
    /// `n` frames of `source` for this block, as its channel count (1 or 2) and its channels (the second ignored for
    /// mono); `None` is silence.
    fn source(&self, source: &SourceRef, n: usize) -> Option<(usize, [&[f32]; 2])>;
}

/// What sees each bus at each stage: the taps live, the capture offline.
pub(crate) trait Points {
    /// `bus` reached `stage`; `channels` hold its `n` frames (the second ignored for a mono bus).
    fn reached(&mut self, stage: Stage, bus: &BusId, channels: usize, block: [&[f32]; 2]);
}

/// The compiled graph and its state.
pub(crate) struct Mix {
    /// Which generation of the control side's graph this is; live setters name it.
    pub generation: u64,
    pub buses: Vec<Bus>,
    rules: Vec<Rule>,
    triggers: Vec<Option<Trigger>>,
    pub outputs: Vec<Output>,
    /// Each bus's `post` from the previous block, averaged to mono, and its length (0 before the first block): what a
    /// sidechain reads.
    previous: Vec<(Vec<f32>, usize)>,
    mic_mode: MicMode,
}

fn internal(message: String) -> AudioError {
    AudioError::Internal(message)
}

/// Moves a sample frame between channel counts (design section 3.6).
fn convert(frame: [f32; 2], from: usize, to: usize) -> [f32; 2] {
    match (from, to) {
        (1, 2) => {
            let side = frame[0] * MONO_TO_STEREO_GAIN;
            [side, side]
        }
        (2, 1) => [(frame[0] + frame[1]) * MONO_TO_STEREO_GAIN, 0.0],
        _ => frame,
    }
}

/// The buses (by id) whose signal can reach an output other than `monitor_out`, following sends.
fn non_monitor_reach(graph: &GraphSpec) -> BTreeSet<BusId> {
    let mut reaching = BTreeSet::new();
    loop {
        let before = reaching.len();
        for bus in &graph.buses {
            let direct = bus.outputs.iter().any(|r| r.as_str() != MONITOR_OUT_ROLE);
            if direct || bus.sends.iter().any(|s| reaching.contains(&s.to)) {
                reaching.insert(bus.id.clone());
            }
        }
        if reaching.len() == before {
            return reaching;
        }
    }
}

fn history_slot(index: u64) -> usize {
    // The remainder is below `HISTORY`, which fits `usize`.
    #[allow(clippy::cast_possible_truncation)]
    {
        (index % HISTORY as u64) as usize
    }
}

/// Whether a rule's trigger was above its threshold `TRIGGER_DELAY` samples before `index`.
fn rule_active(rule: &Rule, triggers: &[Option<Trigger>], index: u64) -> bool {
    match &rule.trigger {
        RuleTrigger::Gate => false,
        RuleTrigger::Bus { bus, threshold } => {
            let Some(back) = index.checked_sub(TRIGGER_DELAY as u64) else { return false };
            triggers[*bus].as_ref().is_some_and(|t| t.levels[history_slot(back)] > *threshold)
        }
    }
}

fn buffers() -> [Vec<f32>; 2] {
    [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]]
}

impl Mix {
    /// A graph with no buses: what the engine runs before its first graph.
    pub(crate) fn empty(generation: u64) -> Self {
        Self {
            generation,
            buses: Vec::new(),
            rules: Vec::new(),
            triggers: Vec::new(),
            outputs: Vec::new(),
            previous: Vec::new(),
            mic_mode: MicMode::Live,
        }
    }

    /// Validates `spec` against the registered node kinds and compiles it, with every ramp at rest at its target for
    /// `mic_mode`: nothing fades in.
    ///
    /// # Errors
    /// [`AudioError::Graph`] for an invalid graph, [`AudioError::Node`] for a node that can't be made.
    pub(crate) fn compile(spec: &GraphSpec, generation: u64, mic_mode: MicMode) -> Result<(Self, NodeIds), AudioError> {
        spec.validate_registered()?;
        let order = spec.bus_order()?;
        let position: BTreeMap<&BusId, usize> = order.iter().enumerate().map(|(i, id)| (id, i)).collect();
        let private_roots = non_monitor_reach(spec);
        let mut outputs: Vec<Output> = Vec::new();
        let mut buses = Vec::with_capacity(order.len());
        let mut ids = Vec::with_capacity(order.len());
        for id in &order {
            let b = spec.bus(id).ok_or_else(|| internal(format!("bus `{id}` vanished while compiling")))?;
            let channels = b.channels.count();
            let chain = compile_chain(b, &position)?;
            let mut sends = Vec::with_capacity(b.sends.len());
            for s in &b.sends {
                let to = *position
                    .get(&s.to)
                    .ok_or_else(|| internal(format!("bus `{id}` sends to the missing bus `{}`", s.to)))?;
                sends.push(Send {
                    to,
                    gain: db_to_gain(s.gain_db),
                    pre_fader: s.pre_fader,
                    ramp: FaderRamp::new(0.0),
                    private_cut: id.as_str() == AI_VOICE_BUS && private_roots.contains(&s.to),
                });
            }
            let mut bus_outputs = Vec::with_capacity(b.outputs.len());
            for role in &b.outputs {
                let at = outputs.iter().position(|o| &o.role == role).unwrap_or_else(|| {
                    outputs.push(Output { role: role.clone(), channels: 1, buf: buffers() });
                    outputs.len() - 1
                });
                outputs[at].channels = outputs[at].channels.max(channels);
                bus_outputs.push(at);
            }
            ids.push((id.clone(), b.chain.iter().map(|n| n.id.clone()).collect()));
            buses.push(Bus {
                id: id.clone(),
                channels,
                sources: b.sources.clone(),
                chain,
                is_mic: id.as_str() == MIC_BUS,
                is_ai_voice: id.as_str() == AI_VOICE_BUS,
                gain: FaderRamp::new(db_to_gain(b.gain_db)),
                mute: FaderRamp::new(1.0),
                private_out: FaderRamp::new(1.0),
                muted: b.muted,
                sends,
                outputs: bus_outputs,
                rules: Vec::new(),
                buf: buffers(),
                post: buffers(),
                meter: Arc::new(MeterCell::new()),
                meter_state: if id.as_str() == PROGRAM_BUS { Meter::program() } else { Meter::plain() },
            });
        }
        let mut mix = Self {
            generation,
            previous: buses.iter().map(|_| (vec![0.0; MAX_BLOCK], 0)).collect(),
            triggers: buses.iter().map(|_| None).collect(),
            buses,
            rules: Vec::new(),
            outputs,
            mic_mode,
        };
        for duck in &spec.ducks {
            let missing = |id: &BusId| internal(format!("a duck rule names the missing bus `{id}`"));
            let target = *position.get(&duck.target).ok_or_else(|| missing(&duck.target))?;
            let trigger = match &duck.trigger {
                DuckTrigger::Bus { id } => {
                    let bus = *position.get(id).ok_or_else(|| missing(id))?;
                    mix.triggers[bus]
                        .get_or_insert_with(|| Trigger { follower: LevelFollower::new(), levels: vec![0.0; HISTORY] });
                    RuleTrigger::Bus { bus, threshold: db_to_gain(duck.threshold_db) }
                }
                DuckTrigger::Gate { .. } => RuleTrigger::Gate,
            };
            mix.buses[target].rules.push(mix.rules.len());
            mix.rules.push(Rule {
                envelope: DuckEnvelope::new(duck.depth_db, duck.attack_ms, duck.hold_ms, duck.release_ms),
                trigger,
            });
        }
        mix.retarget();
        for bus in &mut mix.buses {
            bus.mute = FaderRamp::new(bus.mute.target());
            bus.private_out = FaderRamp::new(bus.private_out.target());
            for send in &mut bus.sends {
                send.ramp = FaderRamp::new(send.ramp.target());
            }
        }
        Ok((mix, ids))
    }

    /// The mute ramp's target for a bus under the current mode: the mic is cut unless it is live.
    fn mute_target(bus: &Bus, mode: MicMode) -> f32 {
        let mode_cut = bus.is_mic && mode != MicMode::Live;
        if bus.muted || mode_cut { 0.0 } else { 1.0 }
    }

    /// Aims every mute, private-output and send ramp at its target for the current mode (a ramp, never a cut).
    fn retarget(&mut self) {
        let mode = self.mic_mode;
        let cohost = mode == MicMode::CohostOnly;
        for bus in &mut self.buses {
            bus.mute.set_target(Self::mute_target(bus, mode));
            bus.private_out.set_target(if cohost && bus.is_ai_voice { 0.0 } else { 1.0 });
            for send in &mut bus.sends {
                send.ramp.set_target(if send.private_cut && cohost { 0.0 } else { send.gain });
            }
        }
    }

    /// Changes the mic mode; the mic's mute and the cut `ai_voice` sends and outputs ramp to their new targets.
    pub(crate) fn set_mic_mode(&mut self, mode: MicMode) {
        self.mic_mode = mode;
        self.retarget();
    }

    /// Aims bus `at`'s fader at the linear `gain`; the ramp restarts from where it is.
    pub(crate) fn set_gain(&mut self, at: usize, gain: f32) {
        if let Some(bus) = self.buses.get_mut(at) {
            bus.gain.set_target(gain);
        }
    }

    /// Mutes or unmutes bus `at` (its own ramp, so unmuting returns to the current gain).
    pub(crate) fn set_muted(&mut self, at: usize, muted: bool) {
        let mode = self.mic_mode;
        if let Some(bus) = self.buses.get_mut(at) {
            bus.muted = muted;
            bus.mute.set_target(Self::mute_target(bus, mode));
        }
    }

    /// Runs one block of `n` frames (at most `MAX_BLOCK`) whose first sample is graph-clock `first`.
    pub(crate) fn process(&mut self, n: usize, first: u64, inputs: &impl Inputs, points: &mut impl Points) {
        let n = n.min(MAX_BLOCK);
        for bus in &mut self.buses {
            bus.buf[0][..n].fill(0.0);
            bus.buf[1][..n].fill(0.0);
        }
        for out in &mut self.outputs {
            out.buf[0][..n].fill(0.0);
            out.buf[1][..n].fill(0.0);
        }
        for at in 0..self.buses.len() {
            let (_, rest) = self.buses.split_at_mut(at);
            let Some((bus, later)) = rest.split_first_mut() else { break };
            add_sources(bus, n, inputs);
            points.reached(Stage::PreChain, &bus.id, bus.channels, [&bus.buf[0][..n], &bus.buf[1][..n]]);
            run_chain(bus, n, first, &self.previous);
            points.reached(Stage::PostChain, &bus.id, bus.channels, [&bus.buf[0][..n], &bus.buf[1][..n]]);
            fade_and_send(bus, at, later, n, first, &mut self.rules, &mut self.triggers, &mut self.outputs);
            let reduction = bus.chain.iter().filter(|l| l.enabled).map(|l| l.node.report().gain_reduction_db).sum();
            let reading = if bus.channels == 2 {
                bus.meter_state.measure(&[&bus.post[0][..n], &bus.post[1][..n]], reduction)
            } else {
                bus.meter_state.measure(&[&bus.post[0][..n]], reduction)
            };
            bus.meter.store(reading);
            points.reached(Stage::PostFader, &bus.id, bus.channels, [&bus.post[0][..n], &bus.post[1][..n]]);
        }
        // A sidechain reads the previous block, so the new ones replace the old only after every bus has run.
        for (bus, (previous, len)) in self.buses.iter().zip(&mut self.previous) {
            for (i, slot) in previous[..n].iter_mut().enumerate() {
                // The pinned arithmetic is the f32 sum, then the halving (design section 3.6).
                #[allow(clippy::manual_midpoint)]
                {
                    *slot = if bus.channels == 2 { (bus.post[0][i] + bus.post[1][i]) / 2.0 } else { bus.post[0][i] };
                }
            }
            *len = n;
        }
    }
}

/// Makes the nodes of `bus`'s chain, each with its parameters set and its hold-over delay.
fn compile_chain(bus: &BusSpec, position: &BTreeMap<&BusId, usize>) -> Result<Vec<Link>, AudioError> {
    let channels = bus.channels.count();
    let mut chain = Vec::with_capacity(bus.chain.len());
    for n in &bus.chain {
        let kind = node_kind(&n.kind)
            .ok_or_else(|| AudioError::Node(NodeError::BadConfig(format!("unknown node kind `{}`", n.kind))))?;
        let info = kind.info();
        let mut node = kind.create(&n.config, bus.channels)?;
        for (name, value) in &n.params {
            let param = info.param_index(name).ok_or_else(|| {
                AudioError::Node(NodeError::BadConfig(format!("`{}` has no parameter `{name}`", n.kind)))
            })?;
            node.set_param(param, *value);
        }
        let sidechain = match &n.sidechain {
            Some(id) => Some(
                *position
                    .get(id)
                    .ok_or_else(|| internal(format!("node `{}` names the missing sidechain bus `{id}`", n.id)))?,
            ),
            None => None,
        };
        let hold = HoldDelay::new(node.latency(), channels);
        chain.push(Link { node, enabled: n.enabled, hold, sidechain });
    }
    Ok(chain)
}

/// Adds the bus's sources into its input (sends from earlier buses are already there).
fn add_sources(bus: &mut Bus, n: usize, inputs: &impl Inputs) {
    let channels = bus.channels;
    let [left, right] = &mut bus.buf;
    for source in &bus.sources {
        let Some((from, block)) = inputs.source(source, n) else { continue };
        for i in 0..n {
            let frame = [block[0][i], if from == 2 { block[1][i] } else { 0.0 }];
            let frame = convert(frame, from, channels);
            left[i] += frame[0];
            if channels == 2 {
                right[i] += frame[1];
            }
        }
    }
}

/// Runs the chain over the bus's input in place.
fn run_chain(bus: &mut Bus, n: usize, first: u64, previous: &[(Vec<f32>, usize)]) {
    let stereo = bus.channels == 2;
    for link in &mut bus.chain {
        let [left, right] = &mut bus.buf;
        let mut block = if stereo {
            BlockMut::stereo(&mut left[..n], &mut right[..n], first)
        } else {
            BlockMut::mono(&mut left[..n], first)
        };
        if let Some((reference, len)) = link.sidechain.and_then(|at| previous.get(at)).filter(|(_, len)| *len > 0) {
            block = block.with_sidechain(&reference[..*len]);
        }
        if link.enabled {
            link.node.process(&mut block);
        } else {
            link.hold.process(&mut block);
        }
    }
}

/// The per-sample part: `post = pre × gain × mute × duck`, the sends into the later buses, the outputs, and the
/// trigger level for a bus that raises a rule.
#[allow(clippy::too_many_arguments)] // The disjoint parts of `Mix` one bus's samples touch.
fn fade_and_send(
    bus: &mut Bus,
    at: usize,
    later: &mut [Bus],
    n: usize,
    first: u64,
    rules: &mut [Rule],
    triggers: &mut [Option<Trigger>],
    outputs: &mut [Output],
) {
    let channels = bus.channels;
    for i in 0..n {
        let index = first + i as u64;
        let gain = bus.gain.next_gain();
        let mute = bus.mute.next_gain();
        let private_out = bus.private_out.next_gain();
        let duck = duck_factor(bus.rules.iter().map(|&r| {
            let active = rule_active(&rules[r], triggers, index);
            rules[r].envelope.step(active)
        }));
        let pre = [bus.buf[0][i], if channels == 2 { bus.buf[1][i] } else { 0.0 }];
        let mut post = [0.0f32; 2];
        for c in 0..channels {
            post[c] = pre[c] * gain * mute * duck;
            bus.post[c][i] = post[c];
        }
        for send in &mut bus.sends {
            let level = send.ramp.next_gain();
            let carried = if send.pre_fader {
                [pre[0] * mute * level, pre[1] * mute * level]
            } else {
                [post[0] * level, post[1] * level]
            };
            // Sends only go to later buses (the order guarantees it); a send to itself or earlier is dropped.
            let Some(target) = send.to.checked_sub(at + 1).and_then(|t| later.get_mut(t)) else { continue };
            let frame = convert(carried, channels, target.channels);
            target.buf[0][i] += frame[0];
            if target.channels == 2 {
                target.buf[1][i] += frame[1];
            }
        }
        for &o in &bus.outputs {
            let Some(out) = outputs.get_mut(o) else { continue };
            let cut = if out.role.as_str() == MONITOR_OUT_ROLE { 1.0 } else { private_out };
            let frame = convert(post, channels, out.channels);
            out.buf[0][i] += frame[0] * cut;
            if out.channels == 2 {
                out.buf[1][i] += frame[1] * cut;
            }
        }
        if let Some(trigger) = triggers[at].as_mut() {
            let level = trigger.follower.step(post[0].abs().max(post[1].abs()));
            trigger.levels[history_slot(index)] = level;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;

    use omuzaki_audio_api::graph::{BusSpec, Channels, DuckRule, NodeSpec, SendSpec};
    use omuzaki_audio_api::ids::{NodeId, NodeKindId};
    use omuzaki_audio_api::node::{
        ChannelSupport, ConfigValue, LatencyRange, NodeConfig, NodeKind, NodeKindInfo, NodeReport, ParamIndex,
    };
    use omuzaki_audio_mock::conformance::{CountingAlloc, allocations_during};

    use super::*;

    #[global_allocator]
    static ALLOC: CountingAlloc = CountingAlloc;

    struct Tone(Vec<f32>);

    impl Inputs for Tone {
        fn source(&self, _: &SourceRef, n: usize) -> Option<(usize, [&[f32]; 2])> {
            Some((1, [&self.0[..n], &self.0[..n]]))
        }
    }

    struct Nowhere;

    impl Points for Nowhere {
        fn reached(&mut self, _: Stage, _: &BusId, _: usize, _: [&[f32]; 2]) {}
    }

    #[test]
    fn a_block_allocates_and_frees_nothing() {
        omuzaki_audio_mock::link();
        let source = SourceRef::Device { role: DeviceRole::new("mic_in").unwrap() };
        let node = |id: &str, enabled| NodeSpec {
            id: NodeId::new(id).unwrap(),
            kind: NodeKindId::new("mock.null").unwrap(),
            config: NodeConfig(BTreeMap::from([("latency".to_owned(), ConfigValue::Int(64))])),
            params: BTreeMap::new(),
            enabled,
            sidechain: None,
        };
        let mic = BusId::new(MIC_BUS).unwrap();
        let music = BusId::new(PROGRAM_BUS).unwrap();
        let graph = GraphSpec {
            buses: vec![
                BusSpec {
                    id: mic.clone(),
                    channels: Channels::Mono,
                    sources: vec![source.clone()],
                    chain: vec![node("on", true), node("off", false)],
                    gain_db: 0.0,
                    muted: false,
                    sends: vec![SendSpec { to: music.clone(), gain_db: -6.0, pre_fader: true }],
                    outputs: vec![DeviceRole::new(MONITOR_OUT_ROLE).unwrap()],
                },
                BusSpec {
                    id: music.clone(),
                    channels: Channels::Stereo,
                    sources: vec![source],
                    chain: Vec::new(),
                    gain_db: -3.0,
                    muted: false,
                    sends: Vec::new(),
                    outputs: vec![DeviceRole::new("main_out").unwrap()],
                },
            ],
            ducks: vec![DuckRule {
                trigger: DuckTrigger::Bus { id: mic },
                target: music,
                depth_db: 12.0,
                threshold_db: -30.0,
                attack_ms: 5.0,
                hold_ms: 50.0,
                release_ms: 100.0,
            }],
            ..GraphSpec::default()
        };
        let (mut mix, _) = Mix::compile(&graph, 1, MicMode::Live).unwrap();
        let tone = Tone((0..MAX_BLOCK).map(|i| (f32::from(u16::try_from(i).unwrap()) * 0.05).sin() * 0.5).collect());
        let mut first = 0u64;
        let count = allocations_during(&mut || {
            for (k, n) in [1024usize, 1, 480, 37, 1024, 256].into_iter().cycle().take(200).enumerate() {
                if k % 17 == 0 {
                    mix.set_gain(1, db_to_gain(-9.0));
                    mix.set_muted(0, k % 34 == 0);
                    mix.set_mic_mode(if k % 51 == 0 { MicMode::CohostOnly } else { MicMode::Live });
                }
                mix.process(n, first, &tone, &mut Nowhere);
                first += n as u64;
            }
        });
        assert_eq!(count, 0, "allocator calls on the audio path");
    }

    /// A node that pulls its input down by a fixed 4 dB and says so, as the program limiter will.
    #[derive(Debug)]
    struct SquashKind;

    omuzaki_audio_api::register_node_kind!(SquashKind);

    struct Squash;

    impl NodeKind for SquashKind {
        fn info(&self) -> NodeKindInfo {
            NodeKindInfo {
                id: "test.squash",
                summary: "pulls the signal down 4 dB and reports it (a test node)",
                channels: ChannelSupport::Any,
                params: &[],
                latency: LatencyRange { min: 0, max: 0 },
                sidechain: false,
            }
        }

        fn create(&self, _: &NodeConfig, _: Channels) -> Result<Box<dyn Node>, NodeError> {
            Ok(Box::new(Squash))
        }
    }

    impl Node for Squash {
        fn latency(&self) -> u32 {
            0
        }

        fn set_param(&mut self, _: ParamIndex, _: f32) {}

        fn process(&mut self, block: &mut BlockMut<'_>) {
            let gain = db_to_gain(-4.0);
            block.for_each_channel(|samples| samples.iter_mut().for_each(|s| *s *= gain));
        }

        fn reset(&mut self) {}

        fn report(&self) -> NodeReport {
            NodeReport { gain_reduction_db: 4.0 }
        }
    }

    #[test]
    fn the_limiters_reduction_is_on_the_program_meter_after_the_first_block() {
        let squash = |enabled| NodeSpec {
            id: NodeId::new("limiter").unwrap(),
            kind: NodeKindId::new("test.squash").unwrap(),
            config: NodeConfig::default(),
            params: BTreeMap::new(),
            enabled,
            sidechain: None,
        };
        let bus = |id: &str, enabled| BusSpec {
            id: BusId::new(id).unwrap(),
            channels: Channels::Stereo,
            sources: vec![SourceRef::Device { role: DeviceRole::new("game_in").unwrap() }],
            chain: vec![squash(enabled)],
            gain_db: 0.0,
            muted: false,
            sends: Vec::new(),
            outputs: Vec::new(),
        };
        let graph = GraphSpec { buses: vec![bus(PROGRAM_BUS, true), bus("game", true)], ..GraphSpec::default() };
        let (mut mix, _) = Mix::compile(&graph, 1, MicMode::Live).unwrap();
        let tone = Tone(vec![0.5; MAX_BLOCK]);
        mix.process(64, 0, &tone, &mut Nowhere);
        let program = mix.buses.iter().find(|b| b.id.as_str() == PROGRAM_BUS).unwrap().meter.load();
        let game = mix.buses.iter().find(|b| b.id.as_str() == "game").unwrap().meter.load();
        assert_eq!(program.limiter_gain_reduction_db, 4.0);
        assert_eq!(game.limiter_gain_reduction_db, 0.0, "only program shows the limiter");
        assert_eq!(game.short_term_lufs, f32::NEG_INFINITY);

        // Switched off, it reports nothing.
        let off = GraphSpec { buses: vec![bus(PROGRAM_BUS, false)], ..GraphSpec::default() };
        let (mut mix, _) = Mix::compile(&off, 1, MicMode::Live).unwrap();
        mix.process(64, 0, &tone, &mut Nowhere);
        assert_eq!(mix.buses[0].meter.load().limiter_gain_reduction_db, 0.0);
    }
}
