//! `fx.eq`: up to eight RBJ bands (issue #1158).
//!
//! Each band has a shape, a frequency, a Q, a gain and a switch. The frequency (as its logarithm, so a sweep moves
//! evenly in pitch), Q and gain move per sample over 20 ms;
//! the coefficients follow them every 32 samples **of the graph clock** (`(first_sample + i) % 32 == 0`), so where the
//! blocks are cut never changes the output. A band's switch crossfades it in or out over 5 ms; its filter keeps
//! running while it is off, so switching it back on starts from the signal, not from a stale state.

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, ConfigValue, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo,
    ParamIndex, ParamSpec, Unit,
};
use omuzaki_audio_api::register_node_kind;

use crate::biquad::{Biquad, Coefs, Shape};
use crate::dsp::{Fade, Smoothed, clamp};

/// The most bands an `fx.eq` node has.
pub const MAX_BANDS: usize = 8;

/// How often the coefficients follow the smoothed parameters, in samples of the graph clock.
pub const COEF_INTERVAL: u64 = 32;

/// Parameters per band: type, freq, q, gain_db, on.
const PER_BAND: usize = 5;

macro_rules! band {
    ($n:literal) => {
        [
            ParamSpec { name: concat!("b", $n, ".type"), unit: Unit::None, min: 0.0, max: 4.0, default: 4.0, smooth_ms: 0.0 },
            ParamSpec { name: concat!("b", $n, ".freq"), unit: Unit::Hz, min: 20.0, max: 20_000.0, default: 1000.0, smooth_ms: 20.0 },
            ParamSpec { name: concat!("b", $n, ".q"), unit: Unit::None, min: 0.1, max: 18.0, default: 0.707, smooth_ms: 20.0 },
            ParamSpec { name: concat!("b", $n, ".gain_db"), unit: Unit::Db, min: -24.0, max: 24.0, default: 0.0, smooth_ms: 20.0 },
            ParamSpec { name: concat!("b", $n, ".on"), unit: Unit::None, min: 0.0, max: 1.0, default: 1.0, smooth_ms: 5.0 },
        ]
    };
}

const fn flatten(bands: [[ParamSpec; PER_BAND]; MAX_BANDS]) -> [ParamSpec; PER_BAND * MAX_BANDS] {
    let mut out = [bands[0][0]; PER_BAND * MAX_BANDS];
    let mut b = 0;
    while b < MAX_BANDS {
        let mut p = 0;
        while p < PER_BAND {
            out[b * PER_BAND + p] = bands[b][p];
            p += 1;
        }
        b += 1;
    }
    out
}

/// The parameters of `fx.eq`: `b1.type` to `b8.on`. Type 0 is low-pass, 1 high-pass, 2 low shelf, 3 high shelf and 4 a
/// bell; every band starts as a flat bell at 1 kHz, so a new EQ changes nothing.
pub static PARAMS: [ParamSpec; PER_BAND * MAX_BANDS] =
    flatten([band!(1), band!(2), band!(3), band!(4), band!(5), band!(6), band!(7), band!(8)]);

/// The `fx.eq` kind. Config: `bands`, 1 to 8 (default 8).
#[derive(Debug)]
pub struct EqKind;

register_node_kind!(EqKind);

impl NodeKind for EqKind {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "fx.eq",
            summary: "an equaliser of up to eight bands: low and high pass, low and high shelf, and bells",
            channels: ChannelSupport::Any,
            params: &PARAMS,
            latency: LatencyRange { min: 0, max: 0 },
            sidechain: false,
        }
    }

    fn create(&self, config: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        let bands = match config.get("bands") {
            None => MAX_BANDS,
            Some(ConfigValue::Int(n)) => usize::try_from(*n)
                .ok()
                .filter(|n| (1..=MAX_BANDS).contains(n))
                .ok_or_else(|| NodeError::BadConfig(format!("bands must be 1 to {MAX_BANDS}, got {n}")))?,
            Some(other) => return Err(NodeError::BadConfig(format!("bands must be a whole number, got {other:?}"))),
        };
        Ok(Box::new(Eq::new(bands, channels.count())))
    }
}

/// One band's controls.
#[derive(Copy, Clone, Debug)]
struct Band {
    shape: Shape,
    freq: Smoothed,
    q: Smoothed,
    gain: Smoothed,
    on: Fade,
    /// One filter per channel.
    filters: [Biquad; 2],
}

impl Band {
    fn new() -> Self {
        let p = |field: usize| f64::from(PARAMS[field].default);
        Self {
            shape: Shape::from_param(p(0)),
            freq: Smoothed::new(p(1).ln(), 20.0),
            q: Smoothed::new(p(2), 20.0),
            gain: Smoothed::new(p(3), 20.0),
            on: Fade::new(p(4) >= 0.5, 5.0),
            filters: [Biquad::default(); 2],
        }
    }

    fn coefs(&self) -> Coefs {
        Coefs::rbj(self.shape, self.freq.value().exp(), self.q.value(), self.gain.value())
    }

    fn retune(&mut self) {
        let coefs = self.coefs();
        for f in &mut self.filters {
            f.coefs = coefs;
        }
    }
}

/// A running `fx.eq`.
#[derive(Clone, Debug)]
pub struct Eq {
    bands: [Band; MAX_BANDS],
    count: usize,
    channels: usize,
}

impl Eq {
    /// An EQ of `bands` flat bands on `channels` channels.
    #[must_use]
    pub fn new(bands: usize, channels: usize) -> Self {
        let mut eq = Self { bands: [Band::new(); MAX_BANDS], count: bands.clamp(1, MAX_BANDS), channels };
        for band in &mut eq.bands {
            band.retune();
        }
        eq
    }
}

impl Node for Eq {
    fn latency(&self) -> u32 {
        0
    }

    fn set_param(&mut self, param: ParamIndex, value: f32) {
        let at = usize::from(param.0);
        let (Some(spec), Some(band)) = (PARAMS.get(at), self.bands.get_mut(at / PER_BAND)) else { return };
        let v = clamp(value, spec.min, spec.max);
        match at % PER_BAND {
            0 => band.shape = Shape::from_param(v),
            1 => band.freq.set(v.ln()),
            2 => band.q.set(v),
            3 => band.gain.set(v),
            _ => band.on.set(v >= 0.5),
        }
    }

    #[allow(clippy::cast_possible_truncation)] // the audio is f32
    fn process(&mut self, block: &mut BlockMut<'_>) {
        let bands = &mut self.bands[..self.count];
        let stereo = self.channels == 2;
        let BlockMut { left, right, first_sample, .. } = block;
        let mut right = right.as_deref_mut();
        for i in 0..left.len() {
            let retune = (*first_sample + i as u64).is_multiple_of(COEF_INTERVAL);
            let mut x = [f64::from(left[i]), right.as_deref().map_or(0.0, |r| f64::from(r[i]))];
            for band in bands.iter_mut() {
                band.freq.next();
                band.q.next();
                band.gain.next();
                let mix = band.on.next();
                if retune {
                    band.retune();
                }
                for (c, x) in x.iter_mut().enumerate().take(if stereo { 2 } else { 1 }) {
                    let wet = band.filters[c].run(*x);
                    *x = if mix >= 1.0 { wet } else { mix.mul_add(wet - *x, *x) };
                }
            }
            left[i] = x[0] as f32;
            if let Some(r) = right.as_deref_mut() {
                r[i] = x[1] as f32;
            }
        }
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            band.freq.snap();
            band.q.snap();
            band.gain.snap();
            band.on.snap();
            band.retune();
            for f in &mut band.filters {
                f.reset();
            }
        }
    }
}
