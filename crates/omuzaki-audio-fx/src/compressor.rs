//! `fx.compressor`: the zero-latency feed-forward compressor with a soft knee (issue #914).
//!
//! The level is the peak, or the RMS over a 10 ms one-pole mean square, of the louder channel (the channels are linked,
//! so a stereo image never shifts). The gain computer is the soft-knee curve of Giannoulis, Massberg and Reiss (2012);
//! its gain reduction is smoothed in dB by a branching one-pole (`attack_ms` while the reduction grows, `release_ms`
//! while it shrinks), and `makeup_db` is added after.

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo, NodeReport,
    ParamIndex, ParamSpec, Unit,
};
use omuzaki_audio_api::register_node_kind;

use crate::dsp::{Smoothed, clamp, gain, one_pole};

/// The RMS detector's averaging time, in milliseconds.
pub const RMS_MS: f64 = 10.0;

/// The parameters of `fx.compressor`. The curve and the times apply at once (the gain they steer moves at the attack
/// and release times); the make-up gain moves over 20 ms.
pub static PARAMS: [ParamSpec; 7] = [
    ParamSpec { name: "threshold_db", unit: Unit::Db, min: -60.0, max: 0.0, default: -18.0, smooth_ms: 0.0 },
    ParamSpec { name: "ratio", unit: Unit::Ratio, min: 1.0, max: 20.0, default: 3.0, smooth_ms: 0.0 },
    ParamSpec { name: "knee_db", unit: Unit::Db, min: 0.0, max: 24.0, default: 6.0, smooth_ms: 0.0 },
    ParamSpec { name: "attack_ms", unit: Unit::Ms, min: 0.1, max: 100.0, default: 5.0, smooth_ms: 0.0 },
    ParamSpec { name: "release_ms", unit: Unit::Ms, min: 10.0, max: 2000.0, default: 120.0, smooth_ms: 0.0 },
    ParamSpec { name: "makeup_db", unit: Unit::Db, min: 0.0, max: 24.0, default: 0.0, smooth_ms: 20.0 },
    ParamSpec { name: "detector", unit: Unit::None, min: 0.0, max: 1.0, default: 1.0, smooth_ms: 0.0 },
];

/// The `fx.compressor` kind. Mono or stereo, linked.
#[derive(Debug)]
pub struct CompressorKind;

register_node_kind!(CompressorKind);

impl NodeKind for CompressorKind {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "fx.compressor",
            summary: "a compressor: evens out loud and quiet speech, with a soft knee and make-up gain",
            channels: ChannelSupport::Any,
            params: &PARAMS,
            latency: LatencyRange { min: 0, max: 0 },
            sidechain: false,
        }
    }

    fn create(&self, _: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        Ok(Box::new(Compressor::new(channels.count())))
    }
}

/// The static curve: the output level for input level `x` (dB), threshold `t`, ratio `r` and knee width `w`.
#[must_use]
pub fn curve(x: f64, t: f64, r: f64, w: f64) -> f64 {
    let over = 2.0 * (x - t);
    if over < -w {
        x
    } else if over.abs() <= w && w > 0.0 {
        let k = x - t + w / 2.0;
        x + (1.0 / r - 1.0) * k * k / (2.0 * w)
    } else {
        t + (x - t) / r
    }
}

/// A running `fx.compressor`.
#[derive(Clone, Debug)]
pub struct Compressor {
    channels: usize,
    threshold: f64,
    ratio: f64,
    knee: f64,
    attack: f64,
    release: f64,
    makeup: Smoothed,
    rms: bool,
    rms_coef: f64,
    mean_square: f64,
    reduction: f64,
}

impl Compressor {
    /// A compressor with the default parameters on `channels` channels.
    #[must_use]
    pub fn new(channels: usize) -> Self {
        let p = |i: usize| f64::from(PARAMS[i].default);
        Self {
            channels,
            threshold: p(0),
            ratio: p(1),
            knee: p(2),
            attack: one_pole(p(3)),
            release: one_pole(p(4)),
            makeup: Smoothed::new(p(5), 20.0),
            rms: p(6) >= 0.5,
            rms_coef: one_pole(RMS_MS),
            mean_square: 0.0,
            reduction: 0.0,
        }
    }

    /// The smoothed gain reduction of the last sample, in dB (0 or less).
    #[must_use]
    pub const fn reduction_db(&self) -> f64 {
        self.reduction
    }

    /// The gain for a sample whose (linked) magnitude is `peak`.
    fn step(&mut self, peak: f64) -> f64 {
        let level = if self.rms {
            self.mean_square = (peak * peak).mul_add(1.0 - self.rms_coef, self.rms_coef * self.mean_square);
            if self.mean_square < 1e-30 {
                self.mean_square = 0.0;
            }
            10.0 * self.mean_square.max(1e-24).log10()
        } else {
            20.0 * peak.max(1e-12).log10()
        };
        let target = curve(level, self.threshold, self.ratio, self.knee) - level;
        let coef = if target < self.reduction { self.attack } else { self.release };
        self.reduction = target + coef * (self.reduction - target);
        if (self.reduction - target).abs() < 1e-12 {
            self.reduction = target;
        }
        gain(self.reduction + self.makeup.next())
    }
}

impl Node for Compressor {
    fn latency(&self) -> u32 {
        0
    }

    fn set_param(&mut self, param: ParamIndex, value: f32) {
        let at = usize::from(param.0);
        let Some(spec) = PARAMS.get(at) else { return };
        let v = clamp(value, spec.min, spec.max);
        match at {
            0 => self.threshold = v,
            1 => self.ratio = v,
            2 => self.knee = v,
            3 => self.attack = one_pole(v),
            4 => self.release = one_pole(v),
            5 => self.makeup.set(v),
            _ => self.rms = v >= 0.5,
        }
    }

    #[allow(clippy::cast_possible_truncation)] // the audio is f32
    fn process(&mut self, block: &mut BlockMut<'_>) {
        let stereo = self.channels == 2;
        let BlockMut { left, right, .. } = block;
        let mut right = right.as_deref_mut();
        for i in 0..left.len() {
            let l = f64::from(left[i]);
            let r = if stereo { right.as_deref().map_or(0.0, |r| f64::from(r[i])) } else { 0.0 };
            let g = self.step(l.abs().max(r.abs()));
            left[i] = (l * g) as f32;
            if let Some(right) = right.as_deref_mut() {
                right[i] = (r * g) as f32;
            }
        }
    }

    fn reset(&mut self) {
        self.mean_square = 0.0;
        self.reduction = 0.0;
        self.makeup.snap();
    }

    fn report(&self) -> NodeReport {
        #[allow(clippy::cast_possible_truncation)] // a meter reading
        NodeReport { gain_reduction_db: (-self.reduction) as f32 }
    }
}
