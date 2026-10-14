//! Nodes: the processing units that sit in a bus's effect chain (design doc section 3.4).
//!
//! A [`NodeKind`] is a registered recipe (`fx.eq`, `model.stream`); a [`Node`] is one running instance. The control
//! thread creates nodes with every buffer they will ever need ([`NodeKind::create`]); the audio thread only calls the
//! [`Node`] methods, which never allocate, free, lock or block.
//!
//! Rules every node follows (the conformance suite in `omuzaki-audio-mock` checks them):
//! 1. **Latency is declared and honest**, and constant while the node is in the graph. A disabled node keeps its
//!    latency (it passes the dry signal delayed by it), so toggling effects never moves the A/V delay.
//! 2. **Block-size invariance**: the same input gives bit-identical output however it is cut into blocks.
//! 3. **No allocation** in [`Node::process`], [`Node::set_param`], [`Node::reset`] or [`Node::report`].
//! 4. **Parameters are smoothed inside the node** (per sample, so block size cannot matter).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::graph::Channels;
use crate::ids::NodeKindId;

/// The rate the whole graph runs at, in Hz: the broadcast rate and the rate every budget is gated at.
pub const SAMPLE_RATE: u32 = 48_000;

/// The most frames [`BlockMut`] ever carries. A device callback larger than this is split into chunks.
pub const MAX_BLOCK: usize = 1024;

/// A parameter's position in [`NodeKindInfo::params`]: the hot-path name of a parameter.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ParamIndex(pub u16);

/// What a parameter's number means, for displays and for agents.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// A bare number or a switch (0 or 1).
    None,
    /// Decibels.
    Db,
    /// Hertz.
    Hz,
    /// Milliseconds.
    Ms,
    /// A ratio such as a compressor's 4:1, stored as 4.
    Ratio,
    /// A fraction of 1, shown as a percentage.
    Percent,
    /// Semitones (pitch shifts).
    Semitones,
}

/// One parameter of a node kind.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ParamSpec {
    /// The name presets and ops use.
    pub name: &'static str,
    /// What the number means.
    pub unit: Unit,
    /// The smallest accepted value.
    pub min: f32,
    /// The largest accepted value.
    pub max: f32,
    /// The value a new node starts with.
    pub default: f32,
    /// How long the node takes to move to a new value, in milliseconds (5 for most gains).
    pub smooth_ms: f32,
}

/// Which channel counts a kind can run on.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelSupport {
    /// Mono buses only (the BR-24 voice effects).
    Mono,
    /// Mono or stereo.
    Any,
}

impl ChannelSupport {
    /// Whether a bus with `channels` can hold this kind.
    #[must_use]
    pub const fn accepts(self, channels: Channels) -> bool {
        matches!((self, channels), (Self::Any, _) | (Self::Mono, Channels::Mono))
    }
}

/// The range a kind's declared latency can take, in samples at 48 kHz. A fixed-latency kind has `min == max`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LatencyRange {
    /// The smallest latency.
    pub min: u32,
    /// The largest latency.
    pub max: u32,
}

/// What a kind says about itself.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct NodeKindInfo {
    /// The kind's id (`fx.eq`). Must be a valid [`NodeKindId`].
    pub id: &'static str,
    /// One line for the effect list and for agents.
    pub summary: &'static str,
    /// The channel counts it runs on.
    pub channels: ChannelSupport,
    /// Its parameters, indexed by [`ParamIndex`].
    pub params: &'static [ParamSpec],
    /// The latency it can declare.
    pub latency: LatencyRange,
    /// Whether it takes a second signal, the referenced bus's previous block (`fx.aec` uses it for its speaker
    /// reference). Only such kinds may carry a `NodeSpec::sidechain`.
    pub sidechain: bool,
}

impl NodeKindInfo {
    /// The index of the parameter called `name`.
    #[must_use]
    pub fn param_index(&self, name: &str) -> Option<ParamIndex> {
        let at = self.params.iter().position(|p| p.name == name)?;
        u16::try_from(at).ok().map(ParamIndex)
    }
}

/// One value of a kind-specific setting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ConfigValue {
    /// A switch.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A real number.
    Float(f64),
    /// Text (a model slot name, a mode).
    Text(String),
}

/// The settings that fix a node's shape and cannot change while it runs (an EQ's band types, a model slot). Changing
/// one is a structural change: the node is recreated. Live knobs are parameters, not config.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct NodeConfig(pub BTreeMap<String, ConfigValue>);

impl NodeConfig {
    /// The value of `key`, if set.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&ConfigValue> {
        self.0.get(key)
    }
}

/// Why a node could not be created.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NodeError {
    /// The config is invalid for this kind.
    #[error("bad node config: {0}")]
    BadConfig(String),
    /// The kind does not run on a bus with this many channels.
    #[error("this node kind does not run on a {0:?} bus")]
    UnsupportedChannels(Channels),
    /// The node refused to start for a reason the person should see (a model that has not been acknowledged).
    #[error("{0}")]
    Refused(String),
}

/// One block of audio, planar and in place, plus where it sits on the graph clock.
///
/// The slices all have the same length, at most [`MAX_BLOCK`] frames.
#[derive(Debug)]
pub struct BlockMut<'a> {
    /// The first (or only) channel.
    pub left: &'a mut [f32],
    /// The second channel of a stereo bus.
    pub right: Option<&'a mut [f32]>,
    /// The graph-clock index of the block's first frame.
    pub first_sample: u64,
    /// For a kind with `NodeKindInfo::sidechain`: the referenced bus's output from the **previous** block, so a
    /// sidechain never makes a cycle. A stereo bus arrives as mono, `(left + right) / 2`. `None` when the node names
    /// no bus, and on the first block. This is the one signal that depends on the block plan: its delay is the length
    /// of the previous block (see [`crate::semantics`]).
    pub sidechain: Option<&'a [f32]>,
}

impl<'a> BlockMut<'a> {
    /// A mono block.
    #[must_use]
    pub fn mono(samples: &'a mut [f32], first_sample: u64) -> Self {
        Self { left: samples, right: None, first_sample, sidechain: None }
    }

    /// A stereo block. `left` and `right` must have the same length.
    #[must_use]
    pub fn stereo(left: &'a mut [f32], right: &'a mut [f32], first_sample: u64) -> Self {
        debug_assert_eq!(left.len(), right.len(), "planar channels have the same length");
        Self { left, right: Some(right), first_sample, sidechain: None }
    }

    /// The same block with a sidechain signal.
    #[must_use]
    pub fn with_sidechain(mut self, sidechain: &'a [f32]) -> Self {
        self.sidechain = Some(sidechain);
        self
    }

    /// Frames in the block.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    /// 1 or 2.
    #[must_use]
    pub fn channel_count(&self) -> usize {
        1 + usize::from(self.right.is_some())
    }

    /// Runs `f` on each channel in turn (left first).
    pub fn for_each_channel(&mut self, mut f: impl FnMut(&mut [f32])) {
        f(self.left);
        if let Some(right) = self.right.as_deref_mut() {
            f(right);
        }
    }
}

/// Numbers a node hands the meters after `process`. `Copy`, empty by default, read on the audio thread.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NodeReport {
    /// How far the node pulled the signal down in the last block, in dB (0 when it did nothing). The program limiter
    /// reports its gain reduction here, and the engine copies it into [`Meters`](crate::report::Meters).
    pub gain_reduction_db: f32,
}

/// One running node. Every method runs on the audio thread (except construction): none may allocate, free, lock,
/// block or log.
pub trait Node: Send {
    /// Samples at 48 kHz by which this node delays its input. Constant while the node is in the graph.
    fn latency(&self) -> u32;

    /// Applies a parameter at a block start. Smooth inside the node; never allocate.
    fn set_param(&mut self, param: ParamIndex, value: f32);

    /// Processes one block in place: planar channels, `frames() <= MAX_BLOCK`.
    fn process(&mut self, block: &mut BlockMut<'_>);

    /// Forgets its history (a new stream). No allocation.
    fn reset(&mut self);

    /// Numbers for the meters, read after `process`. Empty by default.
    fn report(&self) -> NodeReport {
        NodeReport::default()
    }
}

/// A kind of node: its description and its factory.
pub trait NodeKind: Send + Sync {
    /// What this kind says about itself.
    fn info(&self) -> NodeKindInfo;

    /// Control thread: builds a node for a bus with `channels`, with every buffer it will ever need. Parameters start
    /// at their defaults; the compiler then sends `set_param` for the ones the `NodeSpec` sets.
    ///
    /// # Errors
    /// [`NodeError`] when the config or the channel count is invalid, or the node refuses to start.
    fn create(&self, config: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError>;
}

/// One kind's registration, collected by `inventory` at link time. [`register_node_kind!`] builds it.
pub struct NodeKindRegistration {
    /// The kind's name as written in the macro, for messages.
    pub name: &'static str,
    /// The kind. Kinds are stateless factories, so a `'static` value is enough.
    pub kind: &'static dyn NodeKind,
}

impl NodeKindRegistration {
    /// The registration for `kind`. A `const fn` so [`register_node_kind!`] works at link time.
    pub const fn of<T: NodeKind>(name: &'static str, kind: &'static T) -> Self {
        Self { name, kind }
    }
}

inventory::collect!(NodeKindRegistration);

/// Every registered node kind, sorted by id.
#[must_use]
pub fn node_kinds() -> Vec<&'static dyn NodeKind> {
    let mut all: Vec<&'static dyn NodeKind> =
        inventory::iter::<NodeKindRegistration>.into_iter().map(|r| r.kind).collect();
    all.sort_by_key(|k| k.info().id);
    all
}

/// The registered kind called `id`.
#[must_use]
pub fn node_kind(id: &NodeKindId) -> Option<&'static dyn NodeKind> {
    inventory::iter::<NodeKindRegistration>.into_iter().map(|r| r.kind).find(|k| k.info().id == id.as_str())
}

/// Kind ids that more than one registration claims, sorted and listed once. A non-empty answer is a bug in the crates
/// that registered them; the engine refuses to start on it.
#[must_use]
pub fn duplicate_kind_ids() -> Vec<&'static str> {
    let kinds = node_kinds();
    let mut dups: Vec<&'static str> =
        kinds.windows(2).filter(|w| w[0].info().id == w[1].info().id).map(|w| w[0].info().id).collect();
    dups.dedup();
    dups
}

/// Registers a node kind: `register_node_kind!(Eq);`. Put it next to the type. The value must be a constant `'static`
/// expression, normally a unit struct, because it is stored in a link-time registry. The first registration in a crate
/// also needs `omuzaki_core::define_link!();` in its `lib.rs` (ADR 0015).
#[macro_export]
macro_rules! register_node_kind {
    ($kind:expr) => {
        $crate::__inventory::submit! { $crate::node::NodeKindRegistration::of(stringify!($kind), &$kind) }
        $crate::__record_registration!("node_kind", $kind);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Gain;

    impl Node for Gain {
        fn latency(&self) -> u32 {
            0
        }
        fn set_param(&mut self, _: ParamIndex, _: f32) {}
        fn process(&mut self, block: &mut BlockMut<'_>) {
            block.for_each_channel(|c| c.iter_mut().for_each(|s| *s *= 0.5));
        }
        fn reset(&mut self) {}
    }

    struct GainKind;

    impl NodeKind for GainKind {
        fn info(&self) -> NodeKindInfo {
            NodeKindInfo {
                id: "test.half",
                summary: "halves the signal",
                channels: ChannelSupport::Any,
                params: &[ParamSpec {
                    name: "level",
                    unit: Unit::Db,
                    min: -60.0,
                    max: 12.0,
                    default: 0.0,
                    smooth_ms: 5.0,
                }],
                latency: LatencyRange { min: 0, max: 0 },
                sidechain: false,
            }
        }
        fn create(&self, _: &NodeConfig, _: Channels) -> Result<Box<dyn Node>, NodeError> {
            Ok(Box::new(Gain))
        }
    }

    register_node_kind!(GainKind);

    #[test]
    fn a_registered_kind_is_found_by_id_and_creates_a_node() {
        let id = NodeKindId::new("test.half").unwrap();
        let kind = node_kind(&id).expect("registered");
        assert_eq!(kind.info().param_index("level"), Some(ParamIndex(0)));
        assert_eq!(kind.info().param_index("nope"), None);
        let mut node = kind.create(&NodeConfig::default(), Channels::Stereo).unwrap();
        let (mut l, mut r) = ([1.0f32; 4], [2.0f32; 4]);
        let mut block = BlockMut::stereo(&mut l, &mut r, 0);
        assert_eq!((block.frames(), block.channel_count()), (4, 2));
        node.process(&mut block);
        assert_eq!((l, r), ([0.5; 4], [1.0; 4]));
        assert_eq!(node.report(), NodeReport::default());
        assert!(node_kinds().iter().any(|k| k.info().id == "test.half"));
        assert_eq!(duplicate_kind_ids(), Vec::<&str>::new());
    }

    #[test]
    fn channel_support_gates_stereo() {
        assert!(ChannelSupport::Mono.accepts(Channels::Mono) && !ChannelSupport::Mono.accepts(Channels::Stereo));
        assert!(ChannelSupport::Any.accepts(Channels::Stereo));
    }

    #[test]
    fn config_values_round_trip_through_toml() {
        let mut config = NodeConfig::default();
        config.0.insert("slot".into(), ConfigValue::Text("beatrice".into()));
        config.0.insert("bands".into(), ConfigValue::Int(4));
        config.0.insert("rms".into(), ConfigValue::Bool(true));
        config.0.insert("knee".into(), ConfigValue::Float(6.5));
        let text = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<NodeConfig>(&text).unwrap(), config, "{text}");
    }
}
