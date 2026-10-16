//! The "changer" of the clap test: a voice changer reduced to what the test can see, a delay and a different timbre.

use std::f64::consts::{PI, TAU};

use omuzaki_core::rng::Rng;

use super::SAMPLE_RATE;
use super::math::{pow10, sin_cos};

/// A delay followed by a random timbre filter: a peaking band and a low-pass. The clap test finds the delay by
/// matching onsets between a signal and its changed copy, so the copy must be late and must not sound the same.
#[derive(Clone, Debug, PartialEq)]
pub struct Changer {
    /// How many samples the changed signal is late.
    pub delay: usize,
    peak: Biquad,
    low_pass: Biquad,
}

impl Changer {
    /// `signal` delayed by [`Changer::delay`] samples and filtered. The result is `signal.len() + delay + 64` samples long, so nothing
    /// of the tail is cut off: the delay in front, and 64 samples of silence behind for the filters to ring out into.
    #[must_use]
    pub fn apply(&self, signal: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; self.delay];
        out.extend_from_slice(signal);
        out.extend(std::iter::repeat_n(0.0, 64));
        let (mut peak, mut low_pass) = (self.peak.state(), self.low_pass.state());
        for sample in &mut out {
            *sample = low_pass.step(peak.step(*sample));
        }
        out
    }
}

/// A changer for the clap-test pair `seed`: a delay of 20 to 120 ms, a peaking band of ±3 to ±9 dB somewhere from
/// 300 Hz to 3 kHz, and a low-pass from 3 to 8 kHz. The same seed gives the same changer.
#[must_use]
pub fn changer(seed: u64) -> Changer {
    let mut rng = Rng::new(seed);
    let delay = super::samples_in(f64::from(rng.range_f32(0.020, 0.120)));
    let sign = if rng.next_f32() < 0.5 { -1.0 } else { 1.0 };
    let gain_db = sign * f64::from(rng.range_f32(3.0, 9.0));
    let centre = f64::from(rng.range_f32(300.0, 3000.0));
    let q = f64::from(rng.range_f32(0.7, 3.0));
    let cutoff = f64::from(rng.range_f32(3000.0, 8000.0));
    Changer { delay, peak: Biquad::peaking(centre, q, gain_db), low_pass: Biquad::low_pass(cutoff, 0.707) }
}

/// Coefficients of a biquad (RBJ cookbook), normalised so `a0 = 1`.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn peaking(centre: f64, q: f64, gain_db: f64) -> Self {
        let a = pow10(gain_db / 40.0);
        let w = TAU * centre / f64::from(SAMPLE_RATE);
        let (sin_w, cos_w) = sin_cos(w);
        let alpha = sin_w / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        Self {
            b: [(1.0 + alpha * a) / a0, -2.0 * cos_w / a0, (1.0 - alpha * a) / a0],
            a: [-2.0 * cos_w / a0, (1.0 - alpha / a) / a0],
        }
    }

    fn low_pass(cutoff: f64, q: f64) -> Self {
        let w = 2.0 * PI * cutoff / f64::from(SAMPLE_RATE);
        let (sin_w, cos_w) = sin_cos(w);
        let alpha = sin_w / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b: [(1.0 - cos_w) / 2.0 / a0, (1.0 - cos_w) / a0, (1.0 - cos_w) / 2.0 / a0],
            a: [-2.0 * cos_w / a0, (1.0 - alpha) / a0],
        }
    }

    fn state(self) -> BiquadState {
        BiquadState { coefficients: self, x: [0.0; 2], y: [0.0; 2] }
    }
}

/// A biquad with its history (direct form I).
struct BiquadState {
    coefficients: Biquad,
    x: [f64; 2],
    y: [f64; 2],
}

impl BiquadState {
    fn step(&mut self, input: f32) -> f32 {
        let (b, a) = (self.coefficients.b, self.coefficients.a);
        let x = f64::from(input);
        let y = b[0] * x + b[1] * self.x[0] + b[2] * self.x[1] - a[0] * self.y[0] - a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        // An `f32` signal from `f64` arithmetic.
        #[allow(clippy::cast_possible_truncation)]
        {
            y as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::{clap, impulse};

    #[test]
    fn a_changer_is_repeatable_and_differs_between_seeds() {
        assert_eq!(changer(4), changer(4));
        let delays: Vec<usize> = (0..8).map(|s| changer(s).delay).collect();
        assert!(delays.iter().all(|d| (960..=5760).contains(d)), "{delays:?}");
        assert!(delays.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn the_changed_copy_is_late_by_the_delay_and_not_the_same_shape() {
        let c = changer(9);
        let original = clap(4800, 3, 60.0, 1.0);
        let changed = c.apply(&original);
        assert_eq!(changed.len(), original.len() + c.delay + 64);
        assert!(changed[..c.delay].iter().all(|s| *s == 0.0), "nothing before the delay");
        let onset = changed.iter().position(|s| s.abs() > 0.05).unwrap_or(0);
        assert!(onset >= c.delay && onset < c.delay + 64, "onset {onset}, delay {}", c.delay);
        let shifted = &changed[c.delay..c.delay + original.len()];
        assert!(shifted.iter().zip(&original).any(|(a, b)| (a - b).abs() > 0.01), "the timbre changed");
        // A unit impulse comes out finite and bounded: the filters are stable.
        assert!(c.apply(&impulse(4800, 0)).iter().all(|s| s.is_finite() && s.abs() < 4.0));
    }
}
