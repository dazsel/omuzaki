//! The small pieces every effect shares: one-pole coefficients, a per-sample parameter smoother, decibels, a delay line
//! and a counter-based noise generator.

use omuzaki_audio_api::node::SAMPLE_RATE;

/// The sample rate as a float.
pub(crate) const FS: f64 = SAMPLE_RATE as f64;

/// The one-pole coefficient whose step response reaches 63% in `ms` milliseconds: `exp(-1 / (t * fs))`. Zero (no
/// smoothing) for a time of zero or less.
pub(crate) fn one_pole(ms: f64) -> f64 {
    if ms <= 0.0 { 0.0 } else { (-1.0 / (ms / 1000.0 * FS)).exp() }
}

/// Samples in `ms` milliseconds, rounded.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // milliseconds of a voice effect: far below u32
pub(crate) fn samples(ms: f64) -> usize {
    (ms.max(0.0) / 1000.0 * FS).round() as usize
}

/// `10^(db / 20)`.
pub(crate) fn gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// `20 log10(x)`, with a floor so silence is a number.
pub(crate) fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

/// A parameter moving toward its target one sample at a time, so the block size can never matter.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Smoothed {
    value: f64,
    target: f64,
    coef: f64,
}

impl Smoothed {
    /// A parameter at `value`, smoothed with a one-pole of `ms` milliseconds.
    pub(crate) fn new(value: f64, ms: f64) -> Self {
        Self { value, target: value, coef: one_pole(ms) }
    }

    /// Moves toward `target` from now on.
    pub(crate) fn set(&mut self, target: f64) {
        self.target = target;
    }

    /// The next sample's value.
    pub(crate) fn next(&mut self) -> f64 {
        self.value = self.target + self.coef * (self.value - self.target);
        if (self.value - self.target).abs() < 1e-12 {
            self.value = self.target;
        }
        self.value
    }

    /// The current value, without moving.
    pub(crate) const fn value(&self) -> f64 {
        self.value
    }

    /// The target.
    pub(crate) const fn target(&self) -> f64 {
        self.target
    }

    /// Jumps to the target (a reset).
    pub(crate) fn snap(&mut self) {
        self.value = self.target;
    }
}

/// A linear crossfade between 0 and 1 over a fixed time, one sample at a time (switching an effect on or off).
#[derive(Copy, Clone, Debug)]
pub(crate) struct Fade {
    value: f64,
    target: f64,
    step: f64,
}

impl Fade {
    /// A fade at `on` (1 or 0), moving over `ms` milliseconds.
    pub(crate) fn new(on: bool, ms: f64) -> Self {
        let value = if on { 1.0 } else { 0.0 };
        Self { value, target: value, step: 1.0 / (ms / 1000.0 * FS).max(1.0) }
    }

    /// Fades toward on or off.
    pub(crate) fn set(&mut self, on: bool) {
        self.target = if on { 1.0 } else { 0.0 };
    }

    /// The next sample's mix.
    pub(crate) fn next(&mut self) -> f64 {
        if self.value < self.target {
            self.value = (self.value + self.step).min(self.target);
        } else if self.value > self.target {
            self.value = (self.value - self.step).max(self.target);
        }
        self.value
    }

    /// Jumps to the target.
    pub(crate) fn snap(&mut self) {
        self.value = self.target;
    }
}

/// A uniform number in `[-1, 1)` from `seed` and the graph-clock index `n`: the same at the same index however the
/// blocks are cut, and the same on every run (SplitMix64).
pub(crate) fn noise(seed: u64, n: u64) -> f64 {
    let mut z = seed ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    #[allow(clippy::cast_precision_loss)] // 53 bits into an f64 mantissa, exactly
    let unit = (z >> 11) as f64 / (1u64 << 53) as f64;
    unit.mul_add(2.0, -1.0)
}

/// Clamps a parameter to its declared range (the graph validates presets, so this only guards the audio thread).
pub(crate) fn clamp(value: f32, min: f32, max: f32) -> f64 {
    f64::from(if value.is_nan() { min } else { value.clamp(min, max) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_pole_reaches_63_percent_in_its_time() {
        let mut s = Smoothed::new(0.0, 10.0);
        s.set(1.0);
        let n = samples(10.0);
        let mut v = 0.0;
        for _ in 0..n {
            v = s.next();
        }
        assert!((v - (1.0 - (-1.0f64).exp())).abs() < 1e-3, "{v}");
    }

    #[test]
    fn noise_is_the_same_at_the_same_index_and_spread() {
        assert_eq!(noise(7, 1000).to_bits(), noise(7, 1000).to_bits());
        assert_ne!(noise(7, 1000).to_bits(), noise(8, 1000).to_bits());
        let mean = (0..10_000).map(|n| noise(1, n)).sum::<f64>() / 10_000.0;
        assert!(mean.abs() < 0.03, "{mean}");
        assert!((0..10_000).all(|n| (-1.0..1.0).contains(&noise(3, n))));
    }

    #[test]
    fn a_fade_is_linear_and_ends_exactly() {
        let mut f = Fade::new(false, 5.0);
        f.set(true);
        let steps = (0..samples(5.0) + 2).map(|_| f.next()).collect::<Vec<_>>();
        assert!((steps[samples(5.0) / 2] - 0.5).abs() < 0.01);
        assert_eq!(steps.last().copied(), Some(1.0));
    }
}
