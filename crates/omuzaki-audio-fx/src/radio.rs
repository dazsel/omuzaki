//! `fx.radio`: the telephone and radio voices (issue #916).
//!
//! The chain: two high-pass biquads at `low_hz` and two low-pass biquads at `high_hz` (24 dB per octave each side, Q
//! 0.707), a bell at `mid_hz` (`mid_db`, Q 1), the drive `tanh(drive · x) / tanh(drive)`, a bit crush to `bits`
//! (16 is off), a sample-and-hold every `rate_div` samples of the graph clock (1 is off), the noise bed at `noise_db`
//! (-90 is off), and the dry/wet `mix`.
//!
//! `mode` sets the voice's defaults when it changes (0 telephone: 300 to 3,400 Hz, a +3 dB bell at 1.5 kHz, drive 1.5;
//! 1 radio: 200 to 5,000 Hz, +4 dB at 2 kHz, drive 2); the parameters set after it override them.
//!
//! The noise bed is keyed by the voice, the way a transmission's hiss is: it fades in within 5 ms while the input is
//! above -60 dBFS and out over 150 ms after, so silence in stays silence out. It is a counter-based generator on the
//! graph clock (`dsp::noise(seed, sample)`), so it is the same for a seed however the blocks are cut.

use std::f64::consts::FRAC_1_SQRT_2;

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, ConfigValue, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo,
    ParamIndex, ParamSpec, Unit,
};
use omuzaki_audio_api::register_node_kind;

use crate::biquad::{Biquad, Coefs, Shape};
use crate::dsp::{Smoothed, clamp, gain, noise, one_pole};

/// How often the filters follow their smoothed frequencies, in samples of the graph clock.
const COEF_INTERVAL: u64 = 32;

/// The noise key's threshold, attack and release.
const KEY_DB: f64 = -60.0;
const KEY_ATTACK_MS: f64 = 5.0;
const KEY_RELEASE_MS: f64 = 150.0;

/// The seed of the noise bed when the config names none.
pub const DEFAULT_SEED: u64 = 0x0A11_0FF1_CE;

/// Indexes into [`PARAMS`].
mod at {
    pub const MODE: usize = 0;
    pub const LOW: usize = 1;
    pub const HIGH: usize = 2;
    pub const MID: usize = 3;
    pub const MID_DB: usize = 4;
    pub const DRIVE: usize = 5;
    pub const BITS: usize = 6;
    pub const RATE_DIV: usize = 7;
}

/// The parameters of `fx.radio`. The filters, the drive, the noise level and the mix move over 20 ms; the mode, the
/// bit depth and the rate divider are steps by nature and apply at once.
pub static PARAMS: [ParamSpec; 10] = [
    ParamSpec { name: "mode", unit: Unit::None, min: 0.0, max: 1.0, default: 0.0, smooth_ms: 0.0 },
    ParamSpec { name: "low_hz", unit: Unit::Hz, min: 50.0, max: 1000.0, default: 300.0, smooth_ms: 20.0 },
    ParamSpec { name: "high_hz", unit: Unit::Hz, min: 1000.0, max: 12_000.0, default: 3400.0, smooth_ms: 20.0 },
    ParamSpec { name: "mid_hz", unit: Unit::Hz, min: 300.0, max: 5000.0, default: 1500.0, smooth_ms: 20.0 },
    ParamSpec { name: "mid_db", unit: Unit::Db, min: -12.0, max: 12.0, default: 3.0, smooth_ms: 20.0 },
    ParamSpec { name: "drive", unit: Unit::None, min: 1.0, max: 8.0, default: 1.5, smooth_ms: 20.0 },
    ParamSpec { name: "bits", unit: Unit::None, min: 2.0, max: 16.0, default: 16.0, smooth_ms: 0.0 },
    ParamSpec { name: "rate_div", unit: Unit::None, min: 1.0, max: 16.0, default: 1.0, smooth_ms: 0.0 },
    ParamSpec { name: "noise_db", unit: Unit::Db, min: -90.0, max: -20.0, default: -90.0, smooth_ms: 20.0 },
    ParamSpec { name: "mix", unit: Unit::Percent, min: 0.0, max: 1.0, default: 1.0, smooth_ms: 20.0 },
];

/// A voice's defaults: low, high, mid, mid gain, drive.
const VOICES: [[f64; 5]; 2] = [[300.0, 3400.0, 1500.0, 3.0, 1.5], [200.0, 5000.0, 2000.0, 4.0, 2.0]];

/// The `fx.radio` kind. Mono. Config: `seed`, the noise bed's seed (a whole number).
#[derive(Debug)]
pub struct RadioKind;

register_node_kind!(RadioKind);

impl NodeKind for RadioKind {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "fx.radio",
            summary: "a telephone or radio voice: band-limited, driven, optionally crushed, with a keyed noise bed",
            channels: ChannelSupport::Mono,
            params: &PARAMS,
            latency: LatencyRange { min: 0, max: 0 },
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
        Ok(Box::new(Radio::new(seed)))
    }
}

/// How a parameter is held while it moves: frequencies as their logarithm, everything else as is.
fn stored(unit: Unit, value: f64) -> f64 {
    if unit == Unit::Hz { value.ln() } else { value }
}

/// A running `fx.radio`.
#[derive(Clone, Debug)]
pub struct Radio {
    seed: u64,
    mode: usize,
    smooth: [Smoothed; 10],
    bits: f64,
    rate_div: u64,
    filters: [Biquad; 5],
    held: f64,
    key: f64,
    key_attack: f64,
    key_release: f64,
}

impl Radio {
    /// A telephone voice whose noise bed uses `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let mut radio = Self {
            seed,
            mode: 0,
            smooth: PARAMS.map(|p| Smoothed::new(stored(p.unit, f64::from(p.default)), f64::from(p.smooth_ms))),
            bits: f64::from(PARAMS[at::BITS].default),
            rate_div: 1,
            filters: [Biquad::default(); 5],
            held: 0.0,
            key: 0.0,
            key_attack: one_pole(KEY_ATTACK_MS),
            key_release: one_pole(KEY_RELEASE_MS),
        };
        radio.retune();
        radio
    }

    fn retune(&mut self) {
        // The frequencies are smoothed as logarithms, so a sweep moves evenly in pitch and the coefficients take small
        // steps at the low end too.
        let hz = |i: usize| self.smooth[i].value().exp();
        let hp = Coefs::rbj(Shape::HighPass, hz(at::LOW), FRAC_1_SQRT_2, 0.0);
        let lp = Coefs::rbj(Shape::LowPass, hz(at::HIGH), FRAC_1_SQRT_2, 0.0);
        let bell = Coefs::rbj(Shape::Peaking, hz(at::MID), 1.0, self.smooth[at::MID_DB].value());
        for (f, c) in self.filters.iter_mut().zip([hp, hp, lp, lp, bell]) {
            f.coefs = c;
        }
    }

    fn step(&mut self, x: f64, index: u64) -> f64 {
        let [_, _, _, _, _, drive, _, _, noise_db, mix] = self.smooth.each_mut().map(Smoothed::next);
        if index.is_multiple_of(COEF_INTERVAL) {
            self.retune();
        }
        let mut y = self.filters.iter_mut().fold(x, |y, f| f.run(y));
        y = (drive * y).tanh() / drive.tanh();
        if self.bits < 16.0 {
            let levels = 2f64.powf(self.bits - 1.0);
            y = (y * levels).round() / levels;
        }
        if self.rate_div > 1 {
            if index.is_multiple_of(self.rate_div) {
                self.held = y;
            }
            y = self.held;
        }
        let loud = x.abs() > gain(KEY_DB);
        let (target, coef) = if loud { (1.0, self.key_attack) } else { (0.0, self.key_release) };
        self.key = target + coef * (self.key - target);
        if self.key < 1e-9 {
            self.key = 0.0;
        }
        if noise_db > -90.0 && self.key > 0.0 {
            y += gain(noise_db) * self.key * noise(self.seed, index);
        }
        mix.mul_add(y - x, x)
    }
}

impl Node for Radio {
    fn latency(&self) -> u32 {
        0
    }

    fn set_param(&mut self, param: ParamIndex, value: f32) {
        let at = usize::from(param.0);
        let Some(spec) = PARAMS.get(at) else { return };
        let v = clamp(value, spec.min, spec.max);
        match at {
            at::MODE => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0 or 1
                let mode = v.round() as usize;
                if mode != self.mode {
                    self.mode = mode;
                    for (i, value) in [at::LOW, at::HIGH, at::MID, at::MID_DB, at::DRIVE].into_iter().zip(VOICES[mode]) {
                        self.smooth[i].set(stored(PARAMS[i].unit, value));
                    }
                }
            }
            at::BITS => self.bits = v.round(),
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 1 to 16
            at::RATE_DIV => self.rate_div = v.round() as u64,
            _ => self.smooth[at].set(stored(spec.unit, v)),
        }
    }

    #[allow(clippy::cast_possible_truncation)] // the audio is f32
    fn process(&mut self, block: &mut BlockMut<'_>) {
        let first = block.first_sample;
        for (i, s) in block.left.iter_mut().enumerate() {
            *s = self.step(f64::from(*s), first + i as u64) as f32;
        }
    }

    fn reset(&mut self) {
        self.smooth.iter_mut().for_each(Smoothed::snap);
        self.retune();
        self.filters.iter_mut().for_each(Biquad::reset);
        self.held = 0.0;
        self.key = 0.0;
    }
}
