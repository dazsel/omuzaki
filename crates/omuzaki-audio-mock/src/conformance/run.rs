//! Running a node over a signal in blocks.

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{BlockMut, MAX_BLOCK, Node, NodeConfig, NodeKind, NodeKindInfo, ParamIndex};
use omuzaki_core::rng::Rng;

use super::Setting;

/// How a signal is cut into blocks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Every block this long (the last may be shorter).
    Fixed(usize),
    /// Seeded random lengths from 1 to [`MAX_BLOCK`].
    Random(u64),
}

impl Plan {
    /// The block lengths that cover `total` frames.
    pub(crate) fn lengths(self, total: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut left = total;
        let mut rng = match self {
            Self::Random(seed) => Some(Rng::new(seed)),
            Self::Fixed(_) => None,
        };
        while left > 0 {
            let want = match (&mut rng, self) {
                (Some(rng), _) => 1 + usize::try_from(rng.next_u64() % MAX_BLOCK as u64).unwrap_or(0),
                (None, Self::Fixed(n)) => n,
                (None, Self::Random(_)) => 1,
            };
            let len = want.clamp(1, MAX_BLOCK).min(left);
            out.push(len);
            left -= len;
        }
        out
    }
}

impl std::fmt::Display for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fixed(n) => write!(f, "blocks of {n}"),
            Self::Random(seed) => write!(f, "random blocks (seed {seed})"),
        }
    }
}

/// A node made at `setting`: created with `config`, then every parameter set to its default, minimum or maximum.
pub(crate) fn make(
    kind: &dyn NodeKind,
    config: &NodeConfig,
    channels: Channels,
    setting: Setting,
) -> Result<Box<dyn Node>, String> {
    let info = kind.info();
    let mut node = kind.create(config, channels).map_err(|e| format!("create failed on a {channels:?} bus: {e}"))?;
    for (at, spec) in info.params.iter().enumerate() {
        let value = match setting {
            Setting::Default => spec.default,
            Setting::Min => spec.min,
            Setting::Max => spec.max,
        };
        node.set_param(ParamIndex(u16::try_from(at).map_err(|_| "more than 65535 parameters".to_string())?), value);
    }
    Ok(node)
}

/// Samples a parameter needs to settle after it is set: five times its smoothing time, and some.
pub(crate) fn settle_samples(info: &NodeKindInfo) -> usize {
    let longest = info.params.iter().fold(0.0f32, |m, p| m.max(p.smooth_ms));
    // Smoothing times are milliseconds, a few at most; the cast of a small positive float is exact enough.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let smoothing = (f64::from(longest) * 48.0 * 5.0).ceil() as usize;
    smoothing + 64
}

/// Runs `node` over `input` (one vector per channel, equal lengths) cut by `lengths`, with the first block at graph
/// index `first_sample`. Returns the output in the same shape.
pub(crate) fn process(node: &mut dyn Node, input: &[Vec<f32>], lengths: &[usize], first_sample: u64) -> Vec<Vec<f32>> {
    let mut output: Vec<Vec<f32>> = input.iter().map(|c| Vec::with_capacity(c.len())).collect();
    let mut at = 0;
    for &len in lengths {
        let mut left = input[0][at..at + len].to_vec();
        let mut right = input.get(1).map(|c| c[at..at + len].to_vec());
        let first = first_sample + at as u64;
        match right.as_mut() {
            Some(right) => node.process(&mut BlockMut::stereo(&mut left, right, first)),
            None => node.process(&mut BlockMut::mono(&mut left, first)),
        }
        output[0].extend_from_slice(&left);
        if let (Some(right), Some(out)) = (right, output.get_mut(1)) {
            out.extend_from_slice(&right);
        }
        at += len;
    }
    output
}

/// `signal` on every one of `channels` channels.
pub(crate) fn on_channels(signal: &[f32], channels: Channels) -> Vec<Vec<f32>> {
    vec![signal.to_vec(); channels.count()]
}
