//! The mixer graph as data (design doc section 3.1): sources feed buses, a bus runs an effect chain, a fader, ducking
//! and sends, and the `program` bus goes out to devices.
//!
//! It is a mixer, not a free node graph: that is what BR-22 describes, it keeps the UI a mixer strip, and it makes
//! "which captured audio reaches which output" a cheap closure check ([`egress_pairs`]).
//!
//! A [`GraphSpec`] is cold data (serde, JSON Schema, TOML in the project). The control thread validates it
//! ([`GraphSpec::validate`]), compiles it, and swaps the compiled graph in at a block boundary. The arithmetic of the
//! faders, mutes, mono-to-stereo sends and ducking is pinned in [`crate::semantics`].

mod egress;
mod validate;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use egress::{Bindings, CapturedSource, Egress, MicMode, egress_pairs};
pub use validate::{GraphError, KindLookup};

use crate::ids::{BusId, DeviceRole, FeedId, GateId, NodeId, NodeKindId, PlayerId};
use crate::node::NodeConfig;
use crate::tap::VoiceTaps;

/// Where the project document keeps the graph: `audio/graph`, as path segments. The `audio.bus.*` ops write it, the
/// `audio.fx.apply` op writes the `mic` bus's chain in it, and the engine follows it.
pub const GRAPH_DOC_PATH: [&str; 2] = ["audio", "graph"];

/// The bus every voice tap sits on, and the one `MicMode` mutes.
pub const MIC_BUS: &str = "mic";
/// The bus the co-host's voice plays on; in `MicMode::CohostOnly` it reaches `monitor_out` only.
pub const AI_VOICE_BUS: &str = "ai_voice";
/// The bus that goes out to the stream: its meter adds loudness, true peak and the limiter's reduction.
pub const PROGRAM_BUS: &str = "program";
/// The role of the output a person monitors on.
pub const MONITOR_OUT_ROLE: &str = "monitor_out";

/// The highest fader gain, in dB. The lowest is minus infinity (silence).
pub const MAX_GAIN_DB: f32 = 12.0;

/// How many channels a bus carries.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channels {
    /// One channel.
    Mono,
    /// Left and right.
    Stereo,
}

impl Channels {
    /// 1 or 2.
    #[must_use]
    pub const fn count(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }
}

/// Where a bus gets audio from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceRef {
    /// A device input, by role. This is **captured audio** (design doc section 7.2).
    Device {
        /// The role the machine profile binds to a device.
        role: DeviceRole,
    },
    /// A feed written by a producer thread (TTS).
    Feed {
        /// The feed.
        id: FeedId,
    },
    /// A player (soundboard, music).
    Player {
        /// The player.
        id: PlayerId,
    },
}

/// A send from one bus to another.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SendSpec {
    /// The receiving bus.
    pub to: BusId,
    /// The send level in dB; `-inf` cuts the send.
    #[serde(default)]
    pub gain_db: f32,
    /// Taken before the fader: it does not follow the bus gain (a mute still cuts it).
    #[serde(default)]
    pub pre_fader: bool,
}

/// Node parameters by name. Names are stable in files; the hot path uses
/// [`ParamIndex`](crate::node::ParamIndex).
pub type Params = BTreeMap<String, f32>;

fn enabled_by_default() -> bool {
    true
}

/// One node in a bus's chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NodeSpec {
    /// The node, unique within its bus.
    pub id: NodeId,
    /// Its kind.
    pub kind: NodeKindId,
    /// Settings that fix its shape.
    #[serde(default)]
    pub config: NodeConfig,
    /// Parameter values that differ from the defaults.
    #[serde(default)]
    pub params: Params,
    /// Whether it processes. A disabled node still delays its input by its latency.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// For a kind with a sidechain: the bus whose previous-block output it reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<BusId>,
}

/// One bus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BusSpec {
    /// The bus.
    pub id: BusId,
    /// Mono or stereo.
    pub channels: Channels,
    /// What feeds it; the sources are summed.
    #[serde(default)]
    pub sources: Vec<SourceRef>,
    /// The effect chain, in order.
    #[serde(default)]
    pub chain: Vec<NodeSpec>,
    /// The fader, in dB (`-inf` to [`MAX_GAIN_DB`]).
    #[serde(default)]
    pub gain_db: f32,
    /// The fader's mute.
    #[serde(default)]
    pub muted: bool,
    /// Sends to other buses.
    #[serde(default)]
    pub sends: Vec<SendSpec>,
    /// Output roles the bus plays on, after the fader.
    #[serde(default)]
    pub outputs: Vec<DeviceRole>,
}

/// What raises a duck.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DuckTrigger {
    /// A bus's level: its envelope is over the rule's threshold.
    Bus {
        /// The bus.
        id: BusId,
    },
    /// A gate a source raises (a soundboard sound marked "ducks").
    Gate {
        /// The gate.
        id: GateId,
    },
}

/// Lowers `target` by `depth_db` while `trigger` is active. The trigger is read from the **previous block**
/// ([`crate::semantics`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DuckRule {
    /// What raises the duck.
    pub trigger: DuckTrigger,
    /// The bus that is lowered.
    pub target: BusId,
    /// How far to lower it, in dB (positive).
    pub depth_db: f32,
    /// For a bus trigger: the envelope level above which the duck is active, in dB.
    pub threshold_db: f32,
    /// How fast the duck comes in, in ms.
    pub attack_ms: f32,
    /// How long it holds after the trigger ends, in ms.
    pub hold_ms: f32,
    /// How fast it lets go, in ms.
    pub release_ms: f32,
}

/// The whole mixer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphSpec {
    /// The buses, in the order the project lists them.
    #[serde(default)]
    pub buses: Vec<BusSpec>,
    /// The ducking rules.
    #[serde(default)]
    pub ducks: Vec<DuckRule>,
    /// Which signal each voice consumer reads (the section 4.2 defaults unless the person changed them).
    #[serde(default)]
    pub voice_taps: VoiceTaps,
    /// Whether the echo canceller runs.
    #[serde(default)]
    pub aec: AecMode,
}

/// Whether the echo canceller (`fx.aec`) runs (design doc section 4.6).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AecMode {
    /// On when the person says they use speakers or a bound output has `speakers` set; off with headphones.
    #[default]
    Auto,
    /// Always on.
    On,
    /// Always off.
    Off,
}

impl GraphSpec {
    /// The bus called `id`.
    #[must_use]
    pub fn bus(&self, id: &BusId) -> Option<&BusSpec> {
        self.buses.iter().find(|b| &b.id == id)
    }
}
