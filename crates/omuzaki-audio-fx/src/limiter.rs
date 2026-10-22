//! `fx.limiter`: the program bus's transparent true-peak safety limiter, at -1 dBTP and on by default (issue #921,
//! the Director's answer 6 on ADR 0018).
//!
//! **Detection.** Each channel is oversampled 8x by a Kaiser-windowed sinc (16 taps per phase, β 7). The true peak
//! of the stretch between samples `m` and `m + 1` is the largest magnitude of the seven points between them and of the
//! two samples, over both channels (linked); the filter centres that stretch [`DETECTOR_DELAY`] samples back. The
//! issue named the ITU-R BS.1770-4 Annex 2 filter (4x, 12 taps per phase), but that meter's filter under-reads a sine
//! at fs/6 by 0.4 dB (its phases sit unevenly between the samples and droop 0.22 dB at 8 kHz), which let 0.3 dB through
//! on the EBU 3341 fs/6 case; this one under-reads by at most 0.033 dB up to 18 kHz.
//!
//! **Gain.** Sample `m` needs `req[m] = min(1, ceiling / peak)`, where `peak` covers the stretches on both sides of it.
//! A sliding minimum over the [`LOOKAHEAD`] window, `h[m] = min(req[m − W + 1 ..= m])`, followed by a box average of the
//! same length, `a[m] = mean(h[m − W + 1 ..= m])`, gives a gain that ramps down linearly across the window and is at
//! most `req[m − W + 1]` (every `h` in the average includes it), so applying `a[m]` to sample `m − W + 1` meets every
//! peak exactly as it arrives and never overshoots. Release is a one-pole toward 1 (`release_ms`) that the attack can
//! always pull down: `g = min(a, released)`. When nothing needs limiting every value is exactly 1, so the output is the
//! input delayed, bit for bit.
//!
//! **Latency** is [`LATENCY`] samples: the detector's delay plus the window. Switched off (`on` 0), the node passes the
//! input delayed by the same amount, with a 5 ms crossfade, so switching never moves the A/V delay.

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, LatencyRange, Node, NodeConfig, NodeError, NodeKind, NodeKindInfo, NodeReport,
    ParamIndex, ParamSpec, Unit,
};
use omuzaki_audio_api::register_node_kind;

use crate::dsp::{Fade, clamp, db, gain, one_pole};

/// The look-ahead window, in samples (1.5 ms).
pub const LOOKAHEAD: usize = 72;

/// How far back the true-peak filter centres the stretch it measures, in samples.
pub const DETECTOR_DELAY: usize = 8;

/// The declared latency: `DETECTOR_DELAY + LOOKAHEAD - 1` samples (1.65 ms).
pub const LATENCY: u32 = (DETECTOR_DELAY + LOOKAHEAD - 1) as u32;

/// The oversampling factor of the true-peak detector.
pub const OVERSAMPLE: usize = 8;

/// The taps of one phase of the interpolation filter: a windowed sinc 8 samples long on each side.
const TAPS: usize = 16;

/// The Kaiser window's shape.
const KAISER_BETA: f64 = 7.0;

/// The zeroth-order modified Bessel function of the first kind, by its series.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..40 {
        let half = x / (2.0 * f64::from(k));
        term *= half * half;
        sum += term;
        if term < 1e-17 * sum {
            break;
        }
    }
    sum
}

/// The interpolation phases: `phases[p][k]` multiplies `x[n − k]` to give the signal at `n − 8 + p / 8`. Each is a
/// Kaiser-windowed sinc normalised to unity gain at DC. Phase 0 would be `x[n − 8]` itself, which is read directly.
fn interpolator() -> [[f64; TAPS]; OVERSAMPLE] {
    let mut phases = [[0.0; TAPS]; OVERSAMPLE];
    let half = (TAPS / 2) as f64;
    for (p, phase) in phases.iter_mut().enumerate().skip(1) {
        for (k, c) in phase.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)] // tap and phase indexes
            let d = k as f64 - half + p as f64 / OVERSAMPLE as f64;
            let sinc = (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d);
            let r = d / half;
            *c = sinc * bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(KAISER_BETA);
        }
        let sum: f64 = phase.iter().sum();
        phase.iter_mut().for_each(|c| *c /= sum);
    }
    phases
}

/// The parameters of `fx.limiter`. The ceiling and release apply at once (the gain they steer always moves through the
/// look-ahead ramp or the release); the switch crossfades over 5 ms.
pub static PARAMS: [ParamSpec; 3] = [
    ParamSpec { name: "ceiling_dbtp", unit: Unit::Db, min: -6.0, max: 0.0, default: -1.0, smooth_ms: 0.0 },
    ParamSpec { name: "release_ms", unit: Unit::Ms, min: 20.0, max: 500.0, default: 100.0, smooth_ms: 0.0 },
    ParamSpec { name: "on", unit: Unit::None, min: 0.0, max: 1.0, default: 1.0, smooth_ms: 5.0 },
];

/// The `fx.limiter` kind. Mono or stereo, linked.
#[derive(Debug)]
pub struct LimiterKind;

register_node_kind!(LimiterKind);

impl NodeKind for LimiterKind {
    fn info(&self) -> NodeKindInfo {
        NodeKindInfo {
            id: "fx.limiter",
            summary: "the program's safety limiter: keeps the true peak under the ceiling (-1 dBTP), transparent below it",
            channels: ChannelSupport::Any,
            params: &PARAMS,
            latency: LatencyRange { min: LATENCY, max: LATENCY },
            sidechain: false,
        }
    }

    fn create(&self, _: &NodeConfig, channels: Channels) -> Result<Box<dyn Node>, NodeError> {
        Ok(Box::new(Limiter::new(channels.count())))
    }
}

/// The true peak (linear) of the stretch the newest `TAPS` samples of `history` centre, `history[k]` being `x[n − k]`
/// read from a ring at `at`.
fn true_peak(phases: &[[f64; TAPS]; OVERSAMPLE], history: &[f64; TAPS], at: usize) -> f64 {
    let mut peak =
        history[(at + DETECTOR_DELAY) % TAPS].abs().max(history[(at + DETECTOR_DELAY - 1) % TAPS].abs());
    for phase in &phases[1..] {
        let mut sum = 0.0;
        for (k, c) in phase.iter().enumerate() {
            sum = c.mul_add(history[(at + k) % TAPS], sum);
        }
        peak = peak.max(sum.abs());
    }
    peak
}

/// One channel's history and delay line.
#[derive(Clone, Debug)]
struct Channel {
    /// The last `TAPS` inputs; `history[(at + k) % TAPS]` is `x[n − k]`.
    history: [f64; TAPS],
    /// The audio, delayed by `LATENCY`.
    delay: [f32; LATENCY as usize],
}

impl Channel {
    const fn new() -> Self {
        Self { history: [0.0; TAPS], delay: [0.0; LATENCY as usize] }
    }
}

/// A running `fx.limiter`.
#[derive(Clone, Debug)]
pub struct Limiter {
    phases: [[f64; TAPS]; OVERSAMPLE],
    ceiling: f64,
    release: f64,
    on: Fade,
    channels: [Channel; 2],
    count: usize,
    /// Where the newest input sits in `history`, counting down.
    hist_at: usize,
    /// Where the next sample enters (and the oldest leaves) the delay line.
    delay_at: usize,
    last_peak: f64,
    /// The required gains of the last `LOOKAHEAD` samples, and how many are under 1.
    req: [f64; LOOKAHEAD],
    req_under: usize,
    /// The sliding minima of the last `LOOKAHEAD` samples, how many are under 1, and their sum.
    min: [f64; LOOKAHEAD],
    min_under: usize,
    min_sum: f64,
    window_at: usize,
    gain: f64,
    applied: f64,
}

impl Limiter {
    /// A limiter at -1 dBTP on `channels` channels.
    #[must_use]
    pub fn new(channels: usize) -> Self {
        Self {
            phases: interpolator(),
            ceiling: gain(f64::from(PARAMS[0].default)),
            release: one_pole(f64::from(PARAMS[1].default)),
            on: Fade::new(PARAMS[2].default >= 0.5, 5.0),
            channels: [Channel::new(), Channel::new()],
            count: channels.clamp(1, 2),
            hist_at: 0,
            delay_at: 0,
            last_peak: 0.0,
            req: [1.0; LOOKAHEAD],
            req_under: 0,
            min: [1.0; LOOKAHEAD],
            min_under: 0,
            #[allow(clippy::cast_precision_loss)] // 72
            min_sum: LOOKAHEAD as f64,
            window_at: 0,
            gain: 1.0,
            applied: 1.0,
        }
    }

    /// The gain applied to the last sample (1 when nothing was limited).
    #[must_use]
    pub const fn applied_gain(&self) -> f64 {
        self.applied
    }

    /// Takes one frame (`x[..count]`) in and gives the frame `LATENCY` samples older, limited, back in place.
    fn step(&mut self, frame: &mut [f32; 2]) {
        self.hist_at = (self.hist_at + TAPS - 1) % TAPS;
        let mut peak = 0.0f64;
        for (c, ch) in self.channels.iter_mut().enumerate().take(self.count) {
            ch.history[self.hist_at] = f64::from(frame[c]);
            peak = peak.max(true_peak(&self.phases, &ch.history, self.hist_at));
        }
        // Sample m = n − DETECTOR_DELAY borders the stretch measured now and the one measured a sample ago.
        let around = peak.max(self.last_peak);
        self.last_peak = peak;
        let req = if around > self.ceiling { self.ceiling / around } else { 1.0 };

        let w = self.window_at;
        self.req_under = self.req_under + usize::from(req < 1.0) - usize::from(self.req[w] < 1.0);
        self.req[w] = req;
        let h = if self.req_under == 0 { 1.0 } else { self.req.iter().copied().fold(1.0, f64::min) };
        self.min_under = self.min_under + usize::from(h < 1.0) - usize::from(self.min[w] < 1.0);
        self.min_sum += h - self.min[w];
        self.min[w] = h;
        self.window_at = (w + 1) % LOOKAHEAD;
        #[allow(clippy::cast_precision_loss)] // 72
        let average = if self.min_under == 0 {
            // Exactly 1 when nothing in the window is limited, so the sum cannot drift.
            self.min_sum = LOOKAHEAD as f64;
            1.0
        } else {
            self.min_sum / LOOKAHEAD as f64
        };
        let released = 1.0 - self.release * (1.0 - self.gain);
        self.gain = average.min(if 1.0 - released < 1e-12 { 1.0 } else { released });

        let mix = self.on.next();
        self.applied = mix.mul_add(self.gain - 1.0, 1.0);
        let d = self.delay_at;
        for (c, ch) in self.channels.iter_mut().enumerate().take(self.count) {
            let old = std::mem::replace(&mut ch.delay[d], frame[c]);
            #[allow(clippy::cast_possible_truncation)] // the audio is f32
            let out = if self.applied == 1.0 { old } else { (f64::from(old) * self.applied) as f32 };
            frame[c] = out;
        }
        self.delay_at = (d + 1) % LATENCY as usize;
    }
}

impl Node for Limiter {
    fn latency(&self) -> u32 {
        LATENCY
    }

    fn set_param(&mut self, param: ParamIndex, value: f32) {
        let at = usize::from(param.0);
        let Some(spec) = PARAMS.get(at) else { return };
        let v = clamp(value, spec.min, spec.max);
        match at {
            0 => self.ceiling = gain(v),
            1 => self.release = one_pole(v),
            _ => self.on.set(v >= 0.5),
        }
    }

    fn process(&mut self, block: &mut BlockMut<'_>) {
        let BlockMut { left, right, .. } = block;
        let mut right = right.as_deref_mut();
        for i in 0..left.len() {
            let mut frame = [left[i], right.as_deref().map_or(0.0, |r| r[i])];
            self.step(&mut frame);
            left[i] = frame[0];
            if let Some(r) = right.as_deref_mut() {
                r[i] = frame[1];
            }
        }
    }

    fn reset(&mut self) {
        let (ceiling, release, mut on) = (self.ceiling, self.release, self.on);
        on.snap();
        for ch in &mut self.channels {
            *ch = Channel::new();
        }
        *self = Self {
            phases: self.phases,
            ceiling,
            release,
            on,
            channels: self.channels.clone(),
            hist_at: 0,
            delay_at: 0,
            last_peak: 0.0,
            req: [1.0; LOOKAHEAD],
            req_under: 0,
            min: [1.0; LOOKAHEAD],
            min_under: 0,
            #[allow(clippy::cast_precision_loss)] // 72
            min_sum: LOOKAHEAD as f64,
            window_at: 0,
            gain: 1.0,
            applied: 1.0,
            count: self.count,
        };
    }

    fn report(&self) -> NodeReport {
        #[allow(clippy::cast_possible_truncation)] // a meter reading
        NodeReport { gain_reduction_db: (-db(self.applied)) as f32 }
    }
}
