//! The RBJ Audio EQ Cookbook biquads, in transposed direct form II with `f64` state: the filter the EQ, the gate's
//! side-chain and the radio voice share.

use std::f64::consts::PI;

use crate::dsp::FS;

/// The five response shapes, numbered as the `b{n}.type` parameter numbers them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// 12 dB per octave above `freq`.
    LowPass,
    /// 12 dB per octave below `freq`.
    HighPass,
    /// Raises or lowers everything below `freq`.
    LowShelf,
    /// Raises or lowers everything above `freq`.
    HighShelf,
    /// A bell at `freq`.
    Peaking,
}

impl Shape {
    /// The shape a parameter value names (rounded, clamped to 0 to 4).
    #[must_use]
    pub fn from_param(value: f64) -> Self {
        match value.round() {
            v if v < 0.5 => Self::LowPass,
            v if v < 1.5 => Self::HighPass,
            v if v < 2.5 => Self::LowShelf,
            v if v < 3.5 => Self::HighShelf,
            _ => Self::Peaking,
        }
    }
}

/// Normalised biquad coefficients: `b0, b1, b2, a1, a2` (`a0` is 1).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Coefs {
    /// Feed-forward.
    pub b: [f64; 3],
    /// Feedback, without `a0`.
    pub a: [f64; 2],
}

impl Coefs {
    /// The identity filter.
    pub const IDENTITY: Self = Self { b: [1.0, 0.0, 0.0], a: [0.0, 0.0] };

    /// The cookbook coefficients for `shape` at `freq` Hz with quality `q` and (for shelves and bells) `gain_db`.
    #[must_use]
    pub fn rbj(shape: Shape, freq: f64, q: f64, gain_db: f64) -> Self {
        // A bell or shelf of 0 dB is the identity exactly, so a flat band passes the signal bit for bit.
        if gain_db == 0.0 && matches!(shape, Shape::Peaking | Shape::LowShelf | Shape::HighShelf) {
            return Self::IDENTITY;
        }
        // Kept a little under Nyquist so the bilinear transform never folds.
        let w0 = 2.0 * PI * freq.clamp(1.0, FS * 0.49) / FS;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(1e-3));
        let a = 10f64.powf(gain_db / 40.0);
        let (b, a_) = match shape {
            Shape::LowPass => {
                ([(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
            }
            Shape::HighPass => (
                [f64::midpoint(1.0, cos), -(1.0 + cos), f64::midpoint(1.0, cos)],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
            Shape::Peaking => {
                ([1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a], [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a])
            }
            Shape::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    [
                        a * ((a + 1.0) - (a - 1.0) * cos + k),
                        2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                        a * ((a + 1.0) - (a - 1.0) * cos - k),
                    ],
                    [
                        (a + 1.0) + (a - 1.0) * cos + k,
                        -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                        (a + 1.0) + (a - 1.0) * cos - k,
                    ],
                )
            }
            Shape::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    [
                        a * ((a + 1.0) + (a - 1.0) * cos + k),
                        -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                        a * ((a + 1.0) + (a - 1.0) * cos - k),
                    ],
                    [
                        (a + 1.0) - (a - 1.0) * cos + k,
                        2.0 * ((a - 1.0) - (a + 1.0) * cos),
                        (a + 1.0) - (a - 1.0) * cos - k,
                    ],
                )
            }
        };
        let a0 = a_[0];
        Self { b: [b[0] / a0, b[1] / a0, b[2] / a0], a: [a_[1] / a0, a_[2] / a0] }
    }

    /// The magnitude response at `freq` Hz, in dB: the analytic answer the tests compare a filter with.
    #[must_use]
    pub fn magnitude_db(&self, freq: f64) -> f64 {
        let w = 2.0 * PI * freq / FS;
        let (num_re, num_im) = (
            self.b[0] + self.b[1] * w.cos() + self.b[2] * (2.0 * w).cos(),
            -(self.b[1] * w.sin() + self.b[2] * (2.0 * w).sin()),
        );
        let (den_re, den_im) = (
            1.0 + self.a[0] * w.cos() + self.a[1] * (2.0 * w).cos(),
            -(self.a[0] * w.sin() + self.a[1] * (2.0 * w).sin()),
        );
        10.0 * ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).log10()
    }
}

/// One biquad with its state.
#[derive(Copy, Clone, Debug)]
pub struct Biquad {
    /// The coefficients in use.
    pub coefs: Coefs,
    s1: f64,
    s2: f64,
}

impl Default for Biquad {
    fn default() -> Self {
        Self::new(Coefs::IDENTITY)
    }
}

impl Biquad {
    /// A filter at rest with `coefs`.
    #[must_use]
    pub const fn new(coefs: Coefs) -> Self {
        Self { coefs, s1: 0.0, s2: 0.0 }
    }

    /// One sample through, in `f64`.
    pub fn run(&mut self, x: f64) -> f64 {
        let Coefs { b, a } = self.coefs;
        let y = b[0] * x + self.s1;
        self.s1 = b[1] * x - a[0] * y + self.s2;
        self.s2 = b[2] * x - a[1] * y;
        // Denormals and the last crumbs of a decaying tail are flushed, so silence after sound is exact silence.
        if self.s1.abs() < 1e-30 {
            self.s1 = 0.0;
        }
        if self.s2.abs() < 1e-30 {
            self.s2 = 0.0;
        }
        y
    }

    /// One sample through.
    #[allow(clippy::cast_possible_truncation)] // the audio is f32
    pub fn process(&mut self, x: f32) -> f32 {
        self.run(f64::from(x)) as f32
    }

    /// Back to rest.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shapes_have_their_textbook_values() {
        let lp = Coefs::rbj(Shape::LowPass, 1000.0, std::f64::consts::FRAC_1_SQRT_2, 0.0);
        assert!(lp.magnitude_db(10.0).abs() < 0.01);
        assert!((lp.magnitude_db(1000.0) + 3.0103).abs() < 0.01);
        let bell = Coefs::rbj(Shape::Peaking, 2000.0, 1.0, 6.0);
        assert!((bell.magnitude_db(2000.0) - 6.0).abs() < 1e-9);
        let shelf = Coefs::rbj(Shape::HighShelf, 1000.0, 0.707, -12.0);
        assert!((shelf.magnitude_db(20_000.0) + 12.0).abs() < 0.2);
        assert_eq!(Shape::from_param(3.6), Shape::Peaking);
    }
}
