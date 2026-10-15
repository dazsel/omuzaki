//! The reference mixer: the one place the graph is evaluated, written to be obviously correct rather than fast.
//!
//! [`OfflineEngine`](crate::OfflineEngine) drives it over a whole input and [`MockEngine`](crate::MockEngine) drives it
//! block by block from `advance`. It is stateful across blocks (ramps, ducking envelopes, node state, delay lines) and
//! allocates freely: nothing here is real-time code.
//!
//! The recurrences (fader ramps, the level follower, the duck envelope, `db_to_gain`) are the ones in
//! `omuzaki_audio_api::semantics`, which design section 3.6 says both engines share. Routing, ordering, sends, channel
//! conversion, taps and the hold-over of a disabled node's latency are written here independently, so this stays an
//! oracle for them.
//!
//! Per block, for each bus in topological order (a bus after every bus that sends to it):
//! 1. `input` = what the earlier buses sent + the sum of the bus's sources (channel-converted to the bus);
//! 2. the chain: an enabled node processes the block, a disabled one only delays it by its latency;
//! 3. per sample: `post = pre * gain * mute * duck`; sends and outputs are summed into their targets; a bus that
//!    triggers a duck rule feeds its level follower.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_audio_api::engine::{AudioError, Planar};
use omuzaki_audio_api::graph::{
    AI_VOICE_BUS, Channels, DuckTrigger, GraphSpec, MIC_BUS, MONITOR_OUT_ROLE, MicMode, SourceRef,
};
use omuzaki_audio_api::ids::{BusId, DeviceRole, NodeId};
use omuzaki_audio_api::node::{BlockMut, Node, NodeError, ParamIndex, node_kind};
use omuzaki_audio_api::semantics::{
    DuckEnvelope, FaderRamp, LevelFollower, MONO_TO_STEREO_GAIN, TRIGGER_DELAY, db_to_gain, duck_factor,
};
use omuzaki_audio_api::tap::TapPoint;

use crate::null::Delay;

/// Entries in a trigger's level history: twice the trigger delay, so a block of at most `MAX_BLOCK` (= 1,024) samples
/// never overwrites a level a rule still has to read, whatever order the buses run in.
const HISTORY: usize = 2 * TRIGGER_DELAY;

/// One node of a chain, with what the mixer needs beside it.
pub(crate) struct Link {
    pub(crate) id: NodeId,
    pub(crate) node: Box<dyn Node>,
    pub(crate) enabled: bool,
    /// Used instead of the node while it is disabled: the dry signal, `node.latency()` samples late.
    delay: Delay,
    /// The position of the bus whose previous block the node reads, for a kind with a sidechain.
    sidechain: Option<usize>,
}

/// One send, by the position of its target.
pub(crate) struct SendState {
    pub(crate) to: usize,
    gain_db: f32,
    pre_fader: bool,
    /// The send's own ramp: its target is `db_to_gain(gain_db)`, or 0 while the mode cuts it.
    ramp: FaderRamp,
    /// The send leaves `ai_voice` for a bus that reaches an output other than `monitor_out`, so `CohostOnly` cuts it.
    private_cut: bool,
}

/// One bus.
pub(crate) struct Bus {
    pub(crate) id: BusId,
    pub(crate) channels: Channels,
    pub(crate) sources: Vec<SourceRef>,
    pub(crate) chain: Vec<Link>,
    gain: FaderRamp,
    mute: FaderRamp,
    muted: bool,
    pub(crate) sends: Vec<SendState>,
    pub(crate) outputs: Vec<DeviceRole>,
    /// Indices into `Mixer::rules` of the rules that target this bus, in `GraphSpec::ducks` order.
    rules: Vec<usize>,
}

/// What raises a rule.
enum RuleTrigger {
    /// A bus's level history, compared with a threshold gain.
    Bus { bus: usize, threshold: f32 },
    /// A gate. An offline render has no way to raise one, so it never opens.
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

/// What one block produced.
#[derive(Default)]
pub(crate) struct BlockCapture {
    /// Each bus after its fader.
    pub(crate) buses: BTreeMap<BusId, Planar>,
    /// What each output role would have played.
    pub(crate) outputs: BTreeMap<DeviceRole, Planar>,
    /// The raw signal at each tap point that was asked for.
    pub(crate) points: BTreeMap<TapPoint, Planar>,
}

/// The stateful reference mixer.
pub(crate) struct Mixer {
    pub(crate) buses: Vec<Bus>,
    index: BTreeMap<BusId, usize>,
    rules: Vec<Rule>,
    triggers: Vec<Option<Trigger>>,
    output_channels: BTreeMap<DeviceRole, usize>,
    /// Each bus's `post` from the previous block, averaged to mono: what a sidechain reads. `None` before the first.
    previous: Vec<Option<Vec<f32>>>,
    mic_mode: MicMode,
    /// The graph-clock index of the next block's first sample.
    pub(crate) next_sample: u64,
}

/// Moves a sample frame between channel counts: mono to stereo puts `x / sqrt(2)` on each side, stereo to mono is
/// `(L + R) / sqrt(2)`, equal counts pass through (design section 3.6).
pub(crate) fn convert(frame: [f32; 2], from: usize, to: usize) -> [f32; 2] {
    match (from, to) {
        (1, 2) => {
            let side = frame[0] * MONO_TO_STEREO_GAIN;
            [side, side]
        }
        (2, 1) => [(frame[0] + frame[1]) * MONO_TO_STEREO_GAIN, 0.0],
        _ => frame,
    }
}

/// `frames` frames of `source` from `start`: a missing source, or samples past its end, are silence. A source with
/// more than two channels keeps its first two.
pub(crate) fn source_block(audio: Option<&Planar>, start: usize, frames: usize) -> Planar {
    let Some(audio) = audio.filter(|a| !a.is_empty()) else { return vec![vec![0.0; frames]] };
    audio
        .iter()
        .take(2)
        .map(|channel| (start..start + frames).map(|i| channel.get(i).copied().unwrap_or(0.0)).collect())
        .collect()
}

fn frame_of(block: &Planar, n: usize) -> [f32; 2] {
    [block[0][n], block.get(1).map_or(0.0, |c| c[n])]
}

impl Mixer {
    /// Validates `graph` against the registered node kinds, creates its nodes and puts every ramp at rest at its target.
    pub(crate) fn new(graph: &GraphSpec, mic_mode: MicMode) -> Result<Self, AudioError> {
        graph.validate_registered()?;
        let order = graph.bus_order()?;
        let index: BTreeMap<BusId, usize> = order.iter().cloned().enumerate().map(|(i, id)| (id, i)).collect();
        let private_roots = Self::non_monitor_reach(graph);
        let mut buses = Vec::with_capacity(order.len());
        for id in &order {
            let Some(spec) = graph.bus(id) else {
                return Err(AudioError::Internal(format!("bus `{id}` is in the order but not in the graph")));
            };
            let mut chain = Vec::with_capacity(spec.chain.len());
            for node_spec in &spec.chain {
                let kind = node_kind(&node_spec.kind)
                    .ok_or_else(|| AudioError::Internal(format!("kind `{}` vanished", node_spec.kind)))?;
                let info = kind.info();
                let mut node = kind.create(&node_spec.config, spec.channels)?;
                for (name, value) in &node_spec.params {
                    let param = info.param_index(name).ok_or_else(|| {
                        NodeError::BadConfig(format!("`{}` has no parameter `{name}`", node_spec.kind))
                    })?;
                    node.set_param(param, *value);
                }
                let sidechain = node_spec.sidechain.as_ref().and_then(|bus| index.get(bus).copied());
                let delay = Delay::new(node.latency(), spec.channels.count());
                chain.push(Link { id: node_spec.id.clone(), node, enabled: node_spec.enabled, delay, sidechain });
            }
            let sends = spec
                .sends
                .iter()
                .filter_map(|s| index.get(&s.to).map(|to| (s, *to)))
                .map(|(s, to)| SendState {
                    to,
                    gain_db: s.gain_db,
                    pre_fader: s.pre_fader,
                    ramp: FaderRamp::new(0.0),
                    private_cut: id.as_str() == AI_VOICE_BUS && private_roots.contains(&s.to),
                })
                .collect();
            buses.push(Bus {
                id: id.clone(),
                channels: spec.channels,
                sources: spec.sources.clone(),
                chain,
                gain: FaderRamp::new(db_to_gain(spec.gain_db)),
                mute: FaderRamp::new(1.0),
                muted: spec.muted,
                sends,
                outputs: spec.outputs.clone(),
                rules: Vec::new(),
            });
        }
        let mut output_channels: BTreeMap<DeviceRole, usize> = BTreeMap::new();
        for bus in &buses {
            for role in &bus.outputs {
                let entry = output_channels.entry(role.clone()).or_insert(1);
                *entry = (*entry).max(bus.channels.count());
            }
        }
        let mut mixer = Self {
            previous: vec![None; buses.len()],
            triggers: buses.iter().map(|_| None).collect(),
            buses,
            index,
            rules: Vec::new(),
            output_channels,
            mic_mode,
            next_sample: 0,
        };
        mixer.add_rules(graph)?;
        mixer.settle_ramps();
        Ok(mixer)
    }

    /// A mixer with no buses: what an engine runs before its first graph.
    pub(crate) fn empty(mic_mode: MicMode) -> Self {
        Self {
            buses: Vec::new(),
            index: BTreeMap::new(),
            rules: Vec::new(),
            triggers: Vec::new(),
            output_channels: BTreeMap::new(),
            previous: Vec::new(),
            mic_mode,
            next_sample: 0,
        }
    }

    /// The buses (by id) whose signal can reach an output other than `monitor_out`, following sends.
    fn non_monitor_reach(graph: &GraphSpec) -> BTreeSet<BusId> {
        let mut reaching = BTreeSet::new();
        // Repeat until nothing changes: a bus reaches if it has such an output or sends to a bus that does.
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

    fn add_rules(&mut self, graph: &GraphSpec) -> Result<(), AudioError> {
        for duck in &graph.ducks {
            let missing = |id: &BusId| AudioError::Internal(format!("duck rule names the missing bus `{id}`"));
            let target = *self.index.get(&duck.target).ok_or_else(|| missing(&duck.target))?;
            let trigger = match &duck.trigger {
                DuckTrigger::Bus { id } => {
                    let bus = *self.index.get(id).ok_or_else(|| missing(id))?;
                    self.triggers[bus]
                        .get_or_insert_with(|| Trigger { follower: LevelFollower::new(), levels: vec![0.0; HISTORY] });
                    RuleTrigger::Bus { bus, threshold: db_to_gain(duck.threshold_db) }
                }
                DuckTrigger::Gate { .. } => RuleTrigger::Gate,
            };
            self.buses[target].rules.push(self.rules.len());
            self.rules.push(Rule {
                envelope: DuckEnvelope::new(duck.depth_db, duck.attack_ms, duck.hold_ms, duck.release_ms),
                trigger,
            });
        }
        Ok(())
    }

    /// The mute ramp's target for a bus under the current mode: the mic is cut unless it is live.
    fn mute_target(&self, bus: &Bus) -> f32 {
        let mode_cut = bus.id.as_str() == MIC_BUS && self.mic_mode != MicMode::Live;
        if bus.muted || mode_cut { 0.0 } else { 1.0 }
    }

    /// The send ramp's target: `db_to_gain(gain_db)`, or 0 while `CohostOnly` cuts it.
    fn send_target(&self, send: &SendState) -> f32 {
        if send.private_cut && self.mic_mode == MicMode::CohostOnly { 0.0 } else { db_to_gain(send.gain_db) }
    }

    /// Aims every mute and send ramp at its target (a ramp, never a cut) for the current mode.
    fn retarget(&mut self) {
        let mutes: Vec<f32> = self.buses.iter().map(|b| self.mute_target(b)).collect();
        let sends: Vec<Vec<f32>> =
            self.buses.iter().map(|b| b.sends.iter().map(|s| self.send_target(s)).collect()).collect();
        for ((bus, mute), sends) in self.buses.iter_mut().zip(mutes).zip(sends) {
            bus.mute.set_target(mute);
            for (send, target) in bus.sends.iter_mut().zip(sends) {
                send.ramp.set_target(target);
            }
        }
    }

    /// A render starts with every ramp at rest at its target: nothing fades in.
    fn settle_ramps(&mut self) {
        self.retarget();
        for bus in &mut self.buses {
            bus.mute = FaderRamp::new(bus.mute.target());
            for send in &mut bus.sends {
                send.ramp = FaderRamp::new(send.ramp.target());
            }
        }
    }

    /// The position of `bus`, or `UnknownBus`.
    fn position(&self, bus: &BusId) -> Result<usize, AudioError> {
        self.index.get(bus).copied().ok_or_else(|| AudioError::UnknownBus(bus.clone()))
    }

    /// Aims a bus's fader at `db`; the ramp restarts from where it is.
    pub(crate) fn set_bus_gain_db(&mut self, bus: &BusId, db: f32) -> Result<(), AudioError> {
        let at = self.position(bus)?;
        self.buses[at].gain.set_target(db_to_gain(db));
        Ok(())
    }

    /// Mutes or unmutes a bus (its own ramp, so unmuting returns to the current gain).
    pub(crate) fn set_bus_muted(&mut self, bus: &BusId, muted: bool) -> Result<(), AudioError> {
        let at = self.position(bus)?;
        self.buses[at].muted = muted;
        let target = self.mute_target(&self.buses[at]);
        self.buses[at].mute.set_target(target);
        Ok(())
    }

    /// Changes the mic mode; the mic's mute and the cut `ai_voice` sends ramp to their new targets.
    pub(crate) fn set_mic_mode(&mut self, mode: MicMode) {
        self.mic_mode = mode;
        self.retarget();
    }

    /// Sets one node parameter by index.
    pub(crate) fn set_param(
        &mut self,
        bus: &BusId,
        node: &NodeId,
        param: ParamIndex,
        value: f32,
    ) -> Result<(), AudioError> {
        let at = self.position(bus)?;
        let link = self.buses[at]
            .chain
            .iter_mut()
            .find(|l| &l.id == node)
            .ok_or_else(|| AudioError::UnknownNode { bus: bus.clone(), node: node.clone() })?;
        link.node.set_param(param, value);
        Ok(())
    }

    /// Runs the next `frames` frames (at most `MAX_BLOCK`), taking each source's samples from `start`, and returns what
    /// the block produced. `want` lists the raw tap points to capture.
    pub(crate) fn process(
        &mut self,
        sources: &BTreeMap<SourceRef, Planar>,
        start: usize,
        frames: usize,
        want: &BTreeSet<TapPoint>,
    ) -> BlockCapture {
        debug_assert!(frames <= omuzaki_audio_api::MAX_BLOCK, "a block is at most MAX_BLOCK frames");
        let first = self.next_sample;
        let mut capture = BlockCapture::default();
        for point in want {
            if let TapPoint::Input(role) = point {
                let source = SourceRef::Device { role: role.clone() };
                capture.points.insert(point.clone(), source_block(sources.get(&source), start, frames));
            }
        }
        let mut inputs: Vec<Planar> = self.buses.iter().map(|b| vec![vec![0.0; frames]; b.channels.count()]).collect();
        let mut posts: Vec<Option<Planar>> = vec![None; self.buses.len()];
        for at in 0..self.buses.len() {
            let mut pre = std::mem::take(&mut inputs[at]);
            self.add_sources(at, &mut pre, sources, start);
            if want.contains(&TapPoint::BusPreChain(self.buses[at].id.clone())) {
                capture.points.insert(TapPoint::BusPreChain(self.buses[at].id.clone()), pre.clone());
            }
            self.run_chain(at, &mut pre, first);
            if want.contains(&TapPoint::BusPostChain(self.buses[at].id.clone())) {
                capture.points.insert(TapPoint::BusPostChain(self.buses[at].id.clone()), pre.clone());
            }
            let post = self.fade_and_send(at, &pre, first, &mut inputs, &mut capture);
            if want.contains(&TapPoint::BusPostFader(self.buses[at].id.clone())) {
                capture.points.insert(TapPoint::BusPostFader(self.buses[at].id.clone()), post.clone());
            }
            posts[at] = Some(post);
        }
        // A sidechain reads the previous block, so the new ones replace the old only after every bus has run.
        for (at, post) in posts.into_iter().enumerate() {
            let Some(post) = post else { continue };
            self.previous[at] = Some((0..frames).map(|n| frame_mean(&post, n)).collect());
            capture.buses.insert(self.buses[at].id.clone(), post);
        }
        self.next_sample += frames as u64;
        capture
    }

    fn add_sources(&self, at: usize, pre: &mut Planar, sources: &BTreeMap<SourceRef, Planar>, start: usize) {
        let bus = &self.buses[at];
        let frames = pre[0].len();
        for source in &bus.sources {
            let block = source_block(sources.get(source), start, frames);
            for n in 0..frames {
                let frame = convert(frame_of(&block, n), block.len(), bus.channels.count());
                for (c, channel) in pre.iter_mut().enumerate() {
                    channel[n] += frame[c];
                }
            }
        }
    }

    fn run_chain(&mut self, at: usize, pre: &mut Planar, first: u64) {
        let Self { buses, previous, .. } = self;
        for link in &mut buses[at].chain {
            let sidechain = link.sidechain.and_then(|bus| previous[bus].as_deref());
            let mut block = match pre.as_mut_slice() {
                [left, right] => BlockMut::stereo(left, right, first),
                [mono] => BlockMut::mono(mono, first),
                _ => continue,
            };
            if let Some(reference) = sidechain {
                block = block.with_sidechain(reference);
            }
            if link.enabled {
                link.node.process(&mut block);
            } else {
                link.delay.process(&mut block);
            }
        }
    }

    /// The per-sample part: `post = pre * gain * mute * duck`, the sends into `inputs`, the outputs into `capture`, and
    /// the trigger level for a bus that raises a rule. Returns `post`.
    fn fade_and_send(
        &mut self,
        at: usize,
        pre: &Planar,
        first: u64,
        inputs: &mut [Planar],
        capture: &mut BlockCapture,
    ) -> Planar {
        let frames = pre[0].len();
        let Self { buses, rules, triggers, output_channels, .. } = self;
        let channels = buses[at].channels.count();
        let mut post = vec![vec![0.0; frames]; channels];
        let targets: Vec<usize> = buses[at].sends.iter().map(|s| s.to).collect();
        for role in &buses[at].outputs {
            let width = output_channels.get(role).copied().unwrap_or(1);
            capture.outputs.entry(role.clone()).or_insert_with(|| vec![vec![0.0; frames]; width]);
        }
        for n in 0..frames {
            let index = first + n as u64;
            let bus = &mut buses[at];
            let gain = bus.gain.next_gain();
            let mute = bus.mute.next_gain();
            let duck = duck_factor(bus.rules.iter().map(|&r| {
                let active = rule_active(&rules[r], triggers, index);
                rules[r].envelope.step(active)
            }));
            let pre_frame = frame_of(pre, n);
            let mut post_frame = [0.0f32; 2];
            for c in 0..channels {
                post_frame[c] = pre_frame[c] * gain * mute * duck;
                post[c][n] = post_frame[c];
            }
            for (send, &to) in bus.sends.iter_mut().zip(&targets) {
                let level = send.ramp.next_gain();
                let carried = if send.pre_fader {
                    [pre_frame[0] * mute * level, pre_frame[1] * mute * level]
                } else {
                    [post_frame[0] * level, post_frame[1] * level]
                };
                let frame = convert(carried, channels, inputs[to].len());
                for (c, channel) in inputs[to].iter_mut().enumerate() {
                    channel[n] += frame[c];
                }
            }
            for role in &bus.outputs {
                if let Some(out) = capture.outputs.get_mut(role) {
                    let frame = convert(post_frame, channels, out.len());
                    for (c, channel) in out.iter_mut().enumerate() {
                        channel[n] += frame[c];
                    }
                }
            }
            if let Some(trigger) = triggers[at].as_mut() {
                let level = trigger.follower.step(post_frame[0].abs().max(post_frame[1].abs()));
                trigger.levels[history_slot(index)] = level;
            }
        }
        post
    }
}

fn history_slot(index: u64) -> usize {
    // The remainder is below `HISTORY`, which fits `usize`.
    #[allow(clippy::cast_possible_truncation)]
    {
        (index % HISTORY as u64) as usize
    }
}

/// Whether a rule's trigger was active `TRIGGER_DELAY` samples before `index`; false before sample 1,024 of the stream.
fn rule_active(rule: &Rule, triggers: &[Option<Trigger>], index: u64) -> bool {
    match &rule.trigger {
        RuleTrigger::Gate => false,
        RuleTrigger::Bus { bus, threshold } => {
            let Some(back) = index.checked_sub(TRIGGER_DELAY as u64) else { return false };
            triggers[*bus].as_ref().is_some_and(|t| t.levels[history_slot(back)] > *threshold)
        }
    }
}

/// A sample frame as mono for a sidechain: `(L + R) / 2`, or the sample itself. The pinned arithmetic is the f32 sum,
/// then the halving (design section 3.6), which `f32::midpoint` may round differently.
#[allow(clippy::manual_midpoint)]
fn frame_mean(block: &Planar, n: usize) -> f32 {
    match block.as_slice() {
        [left, right] => (left[n] + right[n]) / 2.0,
        [mono] => mono[n],
        _ => 0.0,
    }
}
