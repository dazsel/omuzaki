//! The §10 voice latency gates (#1159): `voice.fx`, the BR-24 chain with every effect on but pitch and formant, adds at
//! most 10 ms (480 samples at 48 kHz); `voice.fx.pitch`, with the shifter on as well, at most 25 ms (1,200 samples).
//! Latency is the chain's declared look-ahead plus one 256-sample block of buffering (spec §10, "Notes").
//!
//! The gate reads what the nodes declare, which the conformance suite holds to what they do. A test kind that declares
//! one sample more than the room left shows the gate fails when it should.

use omuzaki_audio_api::graph::{Channels, NodeSpec};
use omuzaki_audio_api::ids::{NodeId, NodeKindId};
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo, ParamIndex,
};
use omuzaki_audio_api::register_node_kind;
use omuzaki_audio_fx::chain::Chain;
use omuzaki_audio_fx::presets::{budget_chain, budget_chain_with_pitch};

/// The block the gate counts as buffering (spec §10: the audio device's block size, 5.3 ms).
const BLOCK: u32 = 256;
/// `voice.fx`: 10 ms.
const VOICE_FX: u32 = 480;
/// `voice.fx.pitch`: 25 ms.
const VOICE_FX_PITCH: u32 = 1200;

/// The added latency of `chain`, or why it can't be built.
fn added(chain: &[NodeSpec]) -> u32 {
    Chain::build(chain).expect("the chain builds").latency() + BLOCK
}

/// A kind that only delays, by exactly one sample more than the pitched chain leaves.
struct OneTooMany;
register_node_kind!(OneTooMany);

const SPARE: u32 = VOICE_FX_PITCH - BLOCK - 912 + 1;

struct Late;
impl Node for Late {
    fn latency(&self) -> u32 {
        SPARE
    }
    fn set_param(&mut self, _: ParamIndex, _: f32) {}
    fn process(&mut self, _: &mut BlockMut<'_>) {}
    fn reset(&mut self) {}
}

impl NodeKind for OneTooMany {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "test.one_too_many",
            summary: "declares one sample more than the voice budget leaves",
            channels: ChannelSupport::Any,
            params: &[],
            latency: LatencyRange { min: SPARE, max: SPARE },
            sidechain: false,
        }
    }
    fn create(&self, _: &NodeConfig, _: Channels) -> Result<Box<dyn Node>, NodeError> {
        Ok(Box::new(Late))
    }
}

fn late() -> NodeSpec {
    NodeSpec {
        id: NodeId::new("late").unwrap(),
        kind: NodeKindId::new("test.one_too_many").unwrap(),
        config: NodeConfig::default(),
        params: Default::default(),
        enabled: true,
        sidechain: None,
    }
}

#[test]
fn the_voice_chain_adds_at_most_10_ms() {
    let latency = added(&budget_chain());
    eprintln!("voice.fx: {latency} samples ({:.2} ms) against {VOICE_FX}", f64::from(latency) / 48.0);
    assert!(latency <= VOICE_FX);
}

#[test]
fn with_pitch_and_formant_it_adds_at_most_25_ms() {
    let latency = added(&budget_chain_with_pitch());
    eprintln!("voice.fx.pitch: {latency} samples ({:.2} ms) against {VOICE_FX_PITCH}", f64::from(latency) / 48.0);
    assert!(latency <= VOICE_FX_PITCH);
}

#[test]
fn a_kind_declaring_one_sample_too_many_fails_the_gate() {
    let mut chain = budget_chain_with_pitch();
    chain.push(late());
    assert_eq!(added(&chain), VOICE_FX_PITCH + 1);
    assert!(added(&chain) > VOICE_FX_PITCH, "the gate catches it");
    // And a disabled node still counts: switching it off never shifts the bus in time.
    chain.last_mut().unwrap().enabled = false;
    assert_eq!(added(&chain), VOICE_FX_PITCH + 1);
}

#[test]
fn a_disabled_node_delays_by_its_latency_and_an_enabled_chain_by_its_declared_latency() {
    let mut off = budget_chain_with_pitch();
    off.iter_mut().for_each(|n| n.enabled = false);
    let mut chain = Chain::build(&off).unwrap();
    let latency = chain.latency() as usize;
    let mut x = vec![0.0f32; 4096];
    x[100] = 1.0;
    chain.process(&mut x);
    assert_eq!(x.iter().position(|s| *s != 0.0), Some(100 + latency));
}
