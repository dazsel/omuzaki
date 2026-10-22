//! The synthesis half of the shifter (issue #1443): TD-PSOLA on the LPC residual, then the (formant-warped) all-pole
//! filter and the de-emphasis.
//!
//! Each synthesis mark takes the grain of the latest analysis mark at or before it: the residual between that mark's
//! neighbours, under a window that rises from the previous mark and falls to the next (so at α = 1 the grains add back
//! to the residual exactly). Voiced marks are spaced by the analysis spacing over α (or the monotone period); unvoiced
//! ones follow the analysis marks, so noise is never pitch-shifted. Each output sample is filtered by its own hop's
//! predictor, so with β = 1 the filter undoes the analysis exactly; with β ≠ 1 its line spectral frequencies are
//! scaled by β first (at least 50 Hz apart and under 0.98 π), which moves the formants and nothing else.

use std::f64::consts::PI;

use super::analysis::{Analysis, HOP, PRE_EMPHASIS, Ring};
use super::lpc::{IDENTITY, ORDER, Poly, from_lsf, power_gain, warp};
use super::marks::Mark;
use crate::dsp::FS;

/// The widest half-grain, in samples: half of [`LATENCY`], which is what lets a grain end before its residual is
/// needed.
pub const HALF_GRAIN: i64 = 456;

/// The shifter's latency, in samples (19 ms): one grain on each side of its mark. At the F0 floor of 60 Hz (800
/// samples) a mark is confirmed within half a period (400 samples), so a grain's mark is always known in time too.
pub const LATENCY: i64 = 2 * HALF_GRAIN;

/// The closest two warped line spectral frequencies may be, in radians (50 Hz).
const MIN_GAP: f64 = 2.0 * PI * 50.0 / FS;

/// The highest a warped line spectral frequency may go.
const CEILING: f64 = 0.98 * PI;

/// The pitch and formant controls in force when a grain is placed.
#[derive(Copy, Clone, Debug)]
pub struct Shift {
    /// The pitch factor, `2^(semitones / 12)`.
    pub alpha: f64,
    /// How far the monotone period replaces the voice's own: 0 off, 1 fully.
    pub monotone: f64,
    /// The monotone F0, in Hz.
    pub monotone_hz: f64,
    /// The formant factor.
    pub beta: f64,
}

/// Hops over which the formant warp fades in when a voice starts (48 ms): a warped filter switched on at once rings.
const WARP_IN: u32 = 12;

/// How fast the synthesis gain follows a new hop's, in milliseconds.
const GAIN_MS: f64 = 2.0;

/// The synthesis state.
#[derive(Clone, Debug)]
pub struct Synth {
    /// The next synthesis mark, with its fraction, so voiced periods keep sub-sample precision.
    next: f64,
    ola: Ring,
    a: Poly,
    gain: f64,
    gain_target: f64,
    gain_coef: f64,
    history: [f64; 32],
    at: usize,
    de: f64,
    /// Consecutive hops with a warpable frame: the warp fades in over [`WARP_IN`] of them after silence.
    warm: u32,
}

impl Default for Synth {
    fn default() -> Self {
        Self::new()
    }
}

/// `a` with its poles pulled in by `γ`: `a_k γ^k`, which widens every resonance.
fn expand(a: &Poly, gamma: f64) -> Poly {
    let mut out = *a;
    let mut g = 1.0;
    for c in out.iter_mut().skip(1) {
        g *= gamma;
        *c *= g;
    }
    out
}

/// `a` widened (by bisection on `γ` from 0.9 to 1) until its energy is at most `target`, with that energy.
fn widen(a: &Poly, target: f64) -> (Poly, f64) {
    let (mut lo, mut hi) = (0.9, 1.0);
    for _ in 0..14 {
        let mid = 0.5 * (lo + hi);
        if power_gain(&expand(a, mid)).is_none_or(|e| e > target) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let widened = expand(a, lo);
    let energy = power_gain(&widened).unwrap_or(target);
    (widened, energy)
}

impl Synth {
    /// A silent synthesis.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: 0.0,
            ola: Ring::new(4096),
            a: IDENTITY,
            gain: 1.0,
            gain_target: 1.0,
            gain_coef: crate::dsp::one_pole(GAIN_MS),
            history: [0.0; 32],
            at: 0,
            de: 0.0,
            warm: 0,
        }
    }

    /// Back to silence.
    pub fn reset(&mut self) {
        self.next = 0.0;
        self.ola.clear();
        self.a = IDENTITY;
        self.gain = 1.0;
        self.gain_target = 1.0;
        self.history = [0.0; 32];
        self.at = 0;
        self.de = 0.0;
        self.warm = 0;
    }

    /// The output sample at `t` (the analysis has taken every sample up to `t + LATENCY`).
    pub fn next(&mut self, analysis: &Analysis, t: i64, shift: Shift) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        while self.next <= (t + HALF_GRAIN) as f64 {
            self.next = self.place(analysis, self.next, t, shift);
        }
        let u = self.ola.get(t);
        self.ola.set(t, 0.0);
        if t >= 0 && t % HOP == 0 {
            self.retune(analysis, t, shift.beta);
        }
        self.gain = self.gain_target + self.gain_coef * (self.gain - self.gain_target);
        let mut y = u * self.gain;
        for i in 1..=ORDER {
            y -= self.a[i] * self.history[(self.at + 32 - i) & 31];
        }
        if y.abs() < 1e-30 {
            y = 0.0;
        }
        self.history[self.at] = y;
        self.at = (self.at + 1) & 31;
        self.de = PRE_EMPHASIS * self.de + y;
        if self.de.abs() < 1e-30 {
            self.de = 0.0;
        }
        self.de
    }

    /// The predictor for the hop starting at `t`, warped by `beta`, with the gain that keeps the warped filter as loud
    /// as the analysis one (the residual is close to white, so the power gains decide the loudness).
    fn retune(&mut self, analysis: &Analysis, t: i64, beta: f64) {
        let frame = analysis.frame_at(t);
        self.warm = if frame.lsf.is_some() { (self.warm + 1).min(WARP_IN) } else { 0 };
        let beta = 1.0 + (beta - 1.0) * f64::from(self.warm) / f64::from(WARP_IN);
        match frame.lsf {
            Some(lsf) if beta != 1.0 => {
                let warped = from_lsf(&warp(&lsf, beta, MIN_GAP, CEILING));
                // Squeezing the line spectral frequencies together (β < 1) narrows the resonances, by up to 50 dB of
                // energy on a voice whose spectrum falls steeply above 4 kHz: the warped filter is widened (its poles
                // pulled in) until it has no more energy than the original, and then it keeps the original's power for
                // a white excitation, so moving the formants doesn't move the level. Either filter unstable (a
                // degenerate frame): keep the original, unwarped.
                match (power_gain(&frame.a), power_gain(&warped)) {
                    (Some(original), Some(moved)) => {
                        let (a, moved) = if moved > original { widen(&warped, original) } else { (warped, moved) };
                        self.a = a;
                        self.gain_target = (original / moved).sqrt().clamp(1.0 / 256.0, 256.0);
                    }
                    _ => {
                        self.a = frame.a;
                        self.gain_target = 1.0;
                    }
                }
            }
            _ => {
                self.a = frame.a;
                self.gain_target = 1.0;
            }
        }
    }

    /// Places the grain for the synthesis mark `exact` (outputs before `t` are already gone) and returns the next mark.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // sample counts
    fn place(&mut self, analysis: &Analysis, exact: f64, t: i64, shift: Shift) -> f64 {
        let s = exact.round() as i64;
        let marks = &analysis.marks;
        let index = marks.at_or_before(s);
        let mark = index.and_then(|i| marks.get(i)).unwrap_or(Mark { at: s, period: 240.0, voiced: false });
        let before = index.and_then(|i| i.checked_sub(1)).and_then(|i| marks.get(i));
        let after = index.and_then(|i| marks.get(i + 1));
        let period = mark.period.round().max(16.0) as i64;
        let left = before.map_or(period, |b| mark.at - b.at).clamp(16, HALF_GRAIN);
        let right = after.map_or(period, |a| a.at - mark.at).clamp(16, HALF_GRAIN);
        let spacing = after.map_or(mark.period, |a| (a.at - mark.at) as f64).max(16.0);
        if mark.voiced {
            // The period comes from the F0 track, not from the integer spacing of the marks, so the new pitch is as
            // steady as the voice's own.
            let own = mark.period / shift.alpha;
            let step = (1.0 - shift.monotone).mul_add(own, shift.monotone * FS / shift.monotone_hz.max(70.0));
            self.grain(&analysis.e, mark.at, s, t, left, right, (step / spacing).sqrt().clamp(0.5, 1.5));
            exact + step.max(16.0)
        } else {
            // Unvoiced: follow the analysis marks, so the noise keeps its own time and pitch.
            self.grain(&analysis.e, mark.at, s, t, left, right, 1.0);
            after.map_or(exact + spacing, |a| a.at as f64).max(exact + 16.0)
        }
    }

    /// Adds the residual around `from` to the output around `to`, under a window rising over `left` samples and
    /// falling over `right`.
    #[allow(clippy::too_many_arguments, clippy::cast_precision_loss)]
    fn grain(&mut self, e: &Ring, from: i64, to: i64, t: i64, left: i64, right: i64, gain: f64) {
        for (half, sign) in [(left, -1i64), (right, 1)] {
            // cos(π j / half) by rotation, so a grain costs no trigonometry per sample.
            let (sin, cos) = (PI / half as f64).sin_cos();
            let (mut c, mut s) = (1.0f64, 0.0f64);
            let first = if sign < 0 { 1 } else { 0 };
            if first == 1 {
                (c, s) = (c * cos - s * sin, s * cos + c * sin);
            }
            for j in first..half {
                let at = to + sign * j;
                if at >= t {
                    let w = 0.5 * (1.0 + c);
                    self.ola.add(at, gain * w * e.get(from + sign * j));
                }
                (c, s) = (c * cos - s * sin, s * cos + c * sin);
            }
        }
    }
}
