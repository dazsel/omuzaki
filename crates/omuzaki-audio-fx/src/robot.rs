//! `fx.robot`: a ring modulator into a feedback comb, the metallic robot voice (issue #915). The monotone robot is a
//! mode of `fx.pitch`, not this node.
//!
//! `r = x · ((1 − ring_mix) + ring_mix · sin φ)`, with the carrier phase φ an `f64` accumulator that `reset` puts back
//! at zero; then `y[n] = r[n] + feedback · y[n − D]` with `D = comb_ms · 48` samples read by linear interpolation, so a
//! moving `comb_ms` glides instead of clicking; the output is `mix · y + (1 − mix) · x`. Every parameter moves over
//! 20 ms.

use std::f64::consts::TAU;

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo, ParamIndex, ParamSpec,
    Unit,
};
use omuzaki_audio_api::register_node_kind;

use crate::dsp::{FS, Smoothed, clamp};

/// The longest comb delay, in milliseconds.
pub const MAX_COMB_MS: f64 = 12.0;

/// The comb's buffer: the longest delay plus the two samples the interpolation reads.
const COMB_LEN: usize = (MAX_COMB_MS as usize) * 48 + 2;

/// The parameters of `fx.robot`.
pub static PARAMS: [ParamSpec; 5] = [
    ParamSpec { name: "carrier_hz", unit: Unit::Hz, min: 30.0, max: 200.0, default: 60.0, smooth_ms: 20.0 },
    ParamSpec { name: "ring_mix", unit: Unit::Percent, min: 0.0, max: 1.0, default: 1.0, smooth_ms: 20.0 },
    ParamSpec { name: "comb_ms", unit: Unit::Ms, min: 2.0, max: 12.0, default: 6.0, smooth_ms: 20.0 },
    ParamSpec { name: "feedback", unit: Unit::Percent, min: 0.0, max: 0.9, default: 0.5, smooth_ms: 20.0 },
    ParamSpec { name: "mix", unit: Unit::Percent, min: 0.0, max: 1.0, default: 1.0, smooth_ms: 20.0 },
];

/// The `fx.robot` kind. Mono.
#[derive(Debug)]
pub struct RobotKind;

register_node_kind!(RobotKind);

impl NodeKind for RobotKind {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "fx.robot",
            summary: "a robot voice: a ring modulator into a metallic comb",
            channels: ChannelSupport::Mono,
            params: &PARAMS,
            latency: LatencyRange { min: 0, max: 0 },
            sidechain: false,
        }
    }

    fn create(&self, _: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        if channels != Channels::Mono {
            return Err(NodeError::UnsupportedChannels(channels));
        }
        Ok(Box::new(Robot::new()))
    }
}

/// A running `fx.robot`.
#[derive(Clone, Debug)]
pub struct Robot {
    params: [Smoothed; 5],
    phase: f64,
    comb: Box<[f64; COMB_LEN]>,
    at: usize,
}

impl Default for Robot {
    fn default() -> Self {
        Self::new()
    }
}

impl Robot {
    /// A robot with the default parameters.
    #[must_use]
    pub fn new() -> Self {
        Self {
            params: PARAMS.map(|p| Smoothed::new(f64::from(p.default), f64::from(p.smooth_ms))),
            phase: 0.0,
            comb: Box::new([0.0; COMB_LEN]),
            at: 0,
        }
    }

    /// `y[n - delay]`, `delay` in samples (at least 1), interpolated.
    fn read(&self, delay: f64) -> f64 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 96 to 576 samples
        let whole = delay as usize;
        let frac = delay - whole as f64;
        // `comb[at]` is `y[n - 1]`.
        let back = |n: usize| self.comb[(self.at + 1 + COMB_LEN - n) % COMB_LEN];
        frac * (back(whole + 1) - back(whole)) + back(whole)
    }
}

impl Node for Robot {
    fn latency(&self) -> u32 {
        0
    }

    fn set_param(&mut self, param: ParamIndex, value: f32) {
        let at = usize::from(param.0);
        if let (Some(spec), Some(p)) = (PARAMS.get(at), self.params.get_mut(at)) {
            p.set(clamp(value, spec.min, spec.max));
        }
    }

    #[allow(clippy::cast_possible_truncation)] // the audio is f32
    fn process(&mut self, block: &mut BlockMut<'_>) {
        for s in block.left.iter_mut() {
            let [carrier, ring_mix, comb_ms, feedback, mix] = self.params.each_mut().map(Smoothed::next);
            let x = f64::from(*s);
            let r = x * (ring_mix * self.phase.sin() + 1.0 - ring_mix);
            self.phase += TAU * carrier / FS;
            if self.phase >= TAU {
                self.phase -= TAU;
            }
            let mut y = feedback * self.read(comb_ms * FS / 1000.0) + r;
            if y.abs() < 1e-30 {
                y = 0.0;
            }
            self.at = (self.at + 1) % COMB_LEN;
            self.comb[self.at] = y;
            *s = (mix * (y - x) + x) as f32;
        }
    }

    fn reset(&mut self) {
        self.params.iter_mut().for_each(Smoothed::snap);
        self.phase = 0.0;
        self.comb.fill(0.0);
        self.at = 0;
    }
}
