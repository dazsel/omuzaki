//! The `mock.null` node kind: passes audio through, delayed by a configurable latency, with one gain parameter.
//!
//! It exists so tests can build chains whose latency, parameters and sidechain behaviour are known exactly, without a
//! real effect: the latency is the `latency` config (samples), and the output is the input delayed by that many samples
//! times `gain`.

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, ConfigValue, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo,
    ParamIndex, ParamSpec, Unit,
};
use omuzaki_audio_api::register_node_kind;

/// The most latency a `mock.null` node can be given: one second at 48 kHz.
pub const MAX_NULL_LATENCY: u32 = 48_000;

/// The id of the kind.
pub const NULL_KIND_ID: &str = "mock.null";

/// The parameters of `mock.null`: the only one is the gain, applied at once (no smoothing, so a block cut cannot
/// matter when it changes at a block start).
const PARAMS: &[ParamSpec] =
    &[ParamSpec { name: "gain_db", unit: Unit::Db, min: -60.0, max: 12.0, default: 0.0, smooth_ms: 0.0 }];

/// The `mock.null` kind. Config: `latency`, an integer number of samples from 0 to [`MAX_NULL_LATENCY`] (default 0).
#[derive(Debug)]
pub struct NullKind;

register_node_kind!(NullKind);

impl NodeKind for NullKind {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: NULL_KIND_ID,
            summary: "passes audio through, delayed by the configured latency (a test node)",
            channels: ChannelSupport::Any,
            params: PARAMS,
            latency: LatencyRange { min: 0, max: MAX_NULL_LATENCY },
            sidechain: false,
        }
    }

    fn create(&self, config: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        let latency = match config.get("latency") {
            None => 0,
            Some(ConfigValue::Int(n)) => {
                u32::try_from(*n).ok().filter(|n| *n <= MAX_NULL_LATENCY).ok_or_else(|| {
                    NodeError::BadConfig(format!("latency must be 0 to {MAX_NULL_LATENCY} samples, got {n}"))
                })?
            }
            Some(other) => return Err(NodeError::BadConfig(format!("latency must be an integer, got {other:?}"))),
        };
        Ok(Box::new(NullNode::new(latency, channels.count())))
    }
}

/// A delay of `latency` samples per channel, so a disabled node and a `mock.null` node delay identically.
#[derive(Clone, Debug)]
pub(crate) struct Delay {
    /// One history per channel; `history[c][position]` is the oldest sample, the next to leave.
    history: Vec<Vec<f32>>,
    position: usize,
}

impl Delay {
    /// A delay of `latency` samples on `channels` channels, holding silence.
    pub(crate) fn new(latency: u32, channels: usize) -> Self {
        let length = usize::try_from(latency).unwrap_or(usize::MAX);
        Self { history: vec![vec![0.0; length]; channels], position: 0 }
    }

    /// Replaces each sample of `block` with the one `latency` samples before it.
    pub(crate) fn process(&mut self, block: &mut BlockMut<'_>) {
        let length = self.history.first().map_or(0, Vec::len);
        if length == 0 {
            return;
        }
        let start = self.position;
        let mut channel = 0;
        block.for_each_channel(|samples| {
            let mut at = start;
            for sample in samples.iter_mut() {
                std::mem::swap(sample, &mut self.history[channel][at]);
                at = (at + 1) % length;
            }
            channel += 1;
        });
        self.position = (start + block.frames()) % length;
    }

    /// Back to silence.
    pub(crate) fn reset(&mut self) {
        self.history.iter_mut().for_each(|h| h.fill(0.0));
        self.position = 0;
    }
}

/// One `mock.null` instance.
struct NullNode {
    latency: u32,
    delay: Delay,
    gain: f32,
}

impl NullNode {
    fn new(latency: u32, channels: usize) -> Self {
        Self { latency, delay: Delay::new(latency, channels), gain: 1.0 }
    }
}

impl Node for NullNode {
    fn latency(&self) -> u32 {
        self.latency
    }

    fn set_param(&mut self, param: ParamIndex, value: f32) {
        if param == ParamIndex(0) {
            self.gain = omuzaki_audio_api::semantics::db_to_gain(value);
        } else {
            debug_assert!(false, "mock.null has one parameter, got {param:?}");
        }
    }

    fn process(&mut self, block: &mut BlockMut<'_>) {
        self.delay.process(block);
        let gain = self.gain;
        block.for_each_channel(|samples| samples.iter_mut().for_each(|s| *s *= gain));
    }

    fn reset(&mut self) {
        self.delay.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(latency: i64, blocks: &[usize]) -> Vec<f32> {
        let mut config = NodeConfig::default();
        config.0.insert("latency".into(), ConfigValue::Int(latency));
        let mut node = NullKind.create(&config, Channels::Mono).unwrap();
        let mut n = 0u16;
        let mut out = Vec::new();
        let mut at = 0u64;
        for &len in blocks {
            let mut data: Vec<f32> = (0..len)
                .map(|_| {
                    n += 1;
                    f32::from(n)
                })
                .collect();
            node.process(&mut BlockMut::mono(&mut data, at));
            at += len as u64;
            out.extend(data);
        }
        out
    }

    #[test]
    fn the_input_comes_out_latency_samples_late() {
        // Input 1, 2, 3, 4, 5, 6 delayed by 2: silence, silence, then the input from its start.
        assert_eq!(run(2, &[6]), [0.0, 0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn the_block_cut_makes_no_difference() {
        assert_eq!(run(5, &[17]), run(5, &[1, 1, 3, 2, 10]));
        assert_eq!(run(0, &[4]), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn a_bad_latency_is_refused() {
        for bad in
            [ConfigValue::Int(-1), ConfigValue::Int(i64::from(MAX_NULL_LATENCY) + 1), ConfigValue::Text("3".into())]
        {
            let mut config = NodeConfig::default();
            config.0.insert("latency".into(), bad);
            assert!(matches!(NullKind.create(&config, Channels::Mono), Err(NodeError::BadConfig(_))));
        }
    }

    #[test]
    fn the_gain_parameter_scales_and_reset_forgets() {
        let mut node = NullKind.create(&NodeConfig::default(), Channels::Mono).unwrap();
        node.set_param(ParamIndex(0), -20.0);
        let mut data = [1.0f32; 2];
        node.process(&mut BlockMut::mono(&mut data, 0));
        assert!((data[0] - 0.1).abs() < 1e-7);

        let mut delayed = NullKind
            .create(
                &{
                    let mut c = NodeConfig::default();
                    c.0.insert("latency".into(), ConfigValue::Int(3));
                    c
                },
                Channels::Mono,
            )
            .unwrap();
        let mut a = [1.0f32; 2];
        delayed.process(&mut BlockMut::mono(&mut a, 0));
        delayed.reset();
        let mut b = [0.0f32; 4];
        delayed.process(&mut BlockMut::mono(&mut b, 2));
        assert_eq!(b, [0.0; 4]);
    }
}
