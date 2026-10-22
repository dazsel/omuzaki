//! `fx.pitch`: the node kind around the analysis and the synthesis (issue #1443).

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, ConfigValue, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo,
    ParamIndex, ParamSpec, Unit,
};
use omuzaki_audio_api::register_node_kind;

use super::analysis::Analysis;
use super::synth::{LATENCY, Shift, Synth};
use crate::dsp::{Fade, Smoothed, clamp};

/// The parameters of `fx.pitch`, all smoothed over 30 ms. `monotone_hz` 0 is off; from 0 to 70 Hz it fades the
/// monotone in, and from 70 Hz up it is the F0 every voiced period takes.
pub static PARAMS: [ParamSpec; 4] = [
    ParamSpec { name: "semitones", unit: Unit::Semitones, min: -12.0, max: 12.0, default: 0.0, smooth_ms: 30.0 },
    ParamSpec { name: "formant", unit: Unit::Ratio, min: 0.7, max: 1.4, default: 1.0, smooth_ms: 30.0 },
    ParamSpec { name: "monotone_hz", unit: Unit::Hz, min: 0.0, max: 400.0, default: 0.0, smooth_ms: 30.0 },
    ParamSpec { name: "mix", unit: Unit::Percent, min: 0.0, max: 1.0, default: 1.0, smooth_ms: 30.0 },
];

/// The seed of the unvoiced jitter when the config names none.
pub const DEFAULT_SEED: u64 = 0x5050_1A11;

/// The `fx.pitch` kind. Mono. Config: `seed`, the unvoiced marks' jitter seed (a whole number).
#[derive(Debug)]
pub struct PitchKind;

register_node_kind!(PitchKind);

impl NodeKind for PitchKind {
    fn info(&self) -> NodeKindInfo {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 912
        let latency = LATENCY as u32;
        NodeKindInfo {
            id: "fx.pitch",
            summary: "pitch and formant shift without the chipmunk sound, or a monotone robot voice, in 19 ms",
            channels: ChannelSupport::Mono,
            params: &PARAMS,
            latency: LatencyRange { min: latency, max: latency },
            sidechain: false,
        }
    }

    fn create(&self, config: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        if channels != Channels::Mono {
            return Err(NodeError::UnsupportedChannels(channels));
        }
        let seed = match config.get("seed") {
            None => DEFAULT_SEED,
            Some(ConfigValue::Int(n)) => n.unsigned_abs(),
            Some(other) => return Err(NodeError::BadConfig(format!("seed must be a whole number, got {other:?}"))),
        };
        Ok(Box::new(Pitch::new(seed)))
    }
}

/// A running `fx.pitch`.
#[derive(Clone, Debug)]
pub struct Pitch {
    analysis: Analysis,
    synth: Synth,
    params: [Smoothed; 4],
    /// 1 while every parameter is at its identity (the dry signal, delayed, goes out exactly), 0 otherwise.
    identity: Fade,
    /// The level match: the dry and wet signals' short-term powers, and the gain that brings the wet to the dry's.
    level: Level,
}

/// Keeps the shifted voice as loud as the voice it came from. Moving the formants changes how the envelope's peaks
/// line up with the harmonics, which moves the level by several dB either way; the power followers (100 ms) see that
/// and the gain (20 ms) undoes it, within ±12 dB. Near silence both powers sit on the floor and the gain stays put.
#[derive(Clone, Debug)]
struct Level {
    dry: f64,
    wet: f64,
    gain: Smoothed,
    coef: f64,
}

/// The power floor of the level match: -80 dBFS.
const LEVEL_FLOOR: f64 = 1e-8;

impl Level {
    fn new() -> Self {
        Self { dry: 0.0, wet: 0.0, gain: Smoothed::new(1.0, 20.0), coef: crate::dsp::one_pole(100.0) }
    }

    fn apply(&mut self, dry: f64, wet: f64) -> f64 {
        self.dry = self.coef * self.dry + (1.0 - self.coef) * dry * dry;
        self.wet = self.coef * self.wet + (1.0 - self.coef) * wet * wet;
        if self.dry < 1e-30 {
            self.dry = 0.0;
        }
        if self.wet < 1e-30 {
            self.wet = 0.0;
        }
        self.gain.set(((self.dry + LEVEL_FLOOR) / (self.wet + LEVEL_FLOOR)).sqrt().clamp(0.25, 4.0));
        wet * self.gain.next()
    }
}

impl Pitch {
    /// A shifter at its defaults (no shift).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            analysis: Analysis::new(seed),
            synth: Synth::new(),
            params: PARAMS.map(|p| Smoothed::new(f64::from(p.default), f64::from(p.smooth_ms))),
            identity: Fade::new(true, 30.0),
            level: Level::new(),
        }
    }

    /// The analysis, for tests that read its F0 track and marks.
    #[must_use]
    pub const fn analysis(&self) -> &Analysis {
        &self.analysis
    }

    fn at_identity(&self) -> bool {
        self.params[0].target() == 0.0 && self.params[1].target() == 1.0 && self.params[2].target() == 0.0
    }

    /// One sample in, the sample `LATENCY` earlier out.
    pub fn step(&mut self, x: f64) -> f64 {
        let [semitones, formant, monotone_hz, mix] = self.params.each_mut().map(Smoothed::next);
        self.identity.set(self.at_identity());
        let identity = self.identity.next();
        self.analysis.push(x, formant != 1.0 || self.params[1].target() != 1.0);
        let t = self.analysis.now() - 1 - LATENCY;
        let shift = Shift {
            alpha: (semitones / 12.0).exp2(),
            monotone: (monotone_hz / 70.0).min(1.0),
            monotone_hz,
            beta: formant,
        };
        let dry = self.analysis.x.get(t);
        let wet = self.level.apply(dry, self.synth.next(&self.analysis, t, shift));
        let amount = mix * (1.0 - identity);
        if amount == 0.0 { dry } else { amount * (wet - dry) + dry }
    }
}

impl Node for Pitch {
    fn latency(&self) -> u32 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 912
        let latency = LATENCY as u32;
        latency
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
            *s = self.step(f64::from(*s)) as f32;
        }
    }

    fn reset(&mut self) {
        self.analysis.reset();
        self.synth.reset();
        self.params.iter_mut().for_each(Smoothed::snap);
        self.identity.set(self.at_identity());
        self.identity.snap();
        self.level = Level::new();
    }
}
