//! `sin`, `cos`, `exp`, `ln` and `pow10` from nothing but `+ - * /` and exact bit moves.
//!
//! `f64::sin` and its relatives call the platform's math library, which may round the last bit differently on another
//! target. IEEE-754 addition, subtraction, multiplication and division are exact-rounded everywhere, and Rust does not
//! fuse them, so a signal built from these functions is the same on every supported target. They are accurate to about
//! 1e-15 on the ranges the generators use, far finer than an `f32` sample.

// Short names are the maths' own.
#![allow(clippy::many_single_char_names)]

use std::f64::consts::{LN_2, TAU};

/// What `TAU` as an `f64` leaves out of 2 pi, so `x - k * (TAU + TAU_LOW)` loses little for large `x`.
const TAU_LOW: f64 = 2.449_293_598_294_706_4e-16;

/// `sin(x)` and `cos(x)`.
pub(crate) fn sin_cos(x: f64) -> (f64, f64) {
    // Reduce to [-pi, pi], then to [-pi/4, pi/4] by quadrant.
    let turns = (x / TAU).round();
    let r = (x - turns * TAU) - turns * TAU_LOW;
    let quadrant = (r / std::f64::consts::FRAC_PI_2).round();
    let y = (r - quadrant * (TAU / 4.0)) - quadrant * (TAU_LOW / 4.0);
    let (s, c) = (sin_small(y), cos_small(y));
    // `quadrant` is -2..=2.
    match quadrant as i32 {
        0 => (s, c),
        1 => (c, -s),
        -1 => (-c, s),
        _ => (-s, -c),
    }
}

/// `sin(x)`.
pub(crate) fn sin(x: f64) -> f64 {
    sin_cos(x).0
}

/// `cos(x)`.
pub(crate) fn cos(x: f64) -> f64 {
    sin_cos(x).1
}

/// Taylor series of sin on [-pi/4, pi/4]: 13 terms are far past `f64` precision there.
fn sin_small(y: f64) -> f64 {
    let y2 = y * y;
    let mut term = y;
    let mut sum = y;
    for n in 1..=12 {
        let k = f64::from(2 * n) * f64::from(2 * n + 1);
        term *= -y2 / k;
        sum += term;
    }
    sum
}

/// Taylor series of cos on [-pi/4, pi/4].
fn cos_small(y: f64) -> f64 {
    let y2 = y * y;
    let mut term = 1.0;
    let mut sum = 1.0;
    for n in 1..=12 {
        let k = f64::from(2 * n - 1) * f64::from(2 * n);
        term *= -y2 / k;
        sum += term;
    }
    sum
}

/// `e^x`, for `x` within about +-700.
pub(crate) fn exp(x: f64) -> f64 {
    // x = k ln 2 + r with |r| <= ln 2 / 2, so the series converges fast, and 2^k is built exactly from its bits.
    let k = (x / LN_2).round();
    let r = x - k * LN_2;
    let mut term = 1.0;
    let mut sum = 1.0;
    for n in 1..=20 {
        term *= r / f64::from(n);
        sum += term;
    }
    // `k` is within +-1000 by the caller's contract; the exponent field then holds `k + 1023`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let scale = f64::from_bits(((k as i64 + 1023).clamp(1, 2046) as u64) << 52);
    sum * scale
}

/// The natural logarithm of a positive, finite `x`.
pub(crate) fn ln(x: f64) -> f64 {
    // x = m * 2^e with m in [sqrt(1/2), sqrt(2)); ln m = 2 atanh((m - 1) / (m + 1)).
    let bits = x.to_bits();
    let mut e = i64::try_from((bits >> 52) & 0x7FF).unwrap_or(1023) - 1023;
    let mut m = f64::from_bits((bits & ((1 << 52) - 1)) | (1023 << 52));
    if m > std::f64::consts::SQRT_2 {
        m /= 2.0;
        e += 1;
    }
    let z = (m - 1.0) / (m + 1.0);
    let z2 = z * z;
    let mut term = z;
    let mut sum = 0.0;
    for n in 0..30 {
        sum += term / f64::from(2 * n + 1);
        term *= z2;
    }
    // `e` is a small exponent.
    #[allow(clippy::cast_precision_loss)]
    {
        2.0 * sum + e as f64 * LN_2
    }
}

/// `10^x`.
pub(crate) fn pow10(x: f64) -> f64 {
    exp(x * std::f64::consts::LN_10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn they_agree_with_the_standard_library_to_a_millionth_of_a_sample() {
        for i in -4000..4000 {
            let x = f64::from(i) * 0.37;
            assert!((sin(x) - x.sin()).abs() < 1e-12, "sin {x}");
            assert!((cos(x) - x.cos()).abs() < 1e-12, "cos {x}");
        }
        for i in -3000..3000 {
            let x = f64::from(i) * 0.011;
            assert!((exp(x) / x.exp() - 1.0).abs() < 1e-13, "exp {x}");
            assert!((pow10(x) / 10f64.powf(x) - 1.0).abs() < 1e-12, "pow10 {x}");
        }
        for i in 1..5000 {
            let x = f64::from(i) * 0.173;
            assert!((ln(x) - x.ln()).abs() < 1e-12, "ln {x}");
        }
        assert!((ln(1e-3) - 1e-3f64.ln()).abs() < 1e-12);
        let big = 2.0e5;
        assert!((sin(big) - big.sin()).abs() < 1e-11 && (cos(big) - big.cos()).abs() < 1e-11);
    }

    #[test]
    fn the_quadrant_edges_are_exact_enough() {
        for (x, s, c) in [(0.0, 0.0, 1.0), (std::f64::consts::FRAC_PI_2, 1.0, 0.0), (std::f64::consts::PI, 0.0, -1.0)] {
            let (a, b) = sin_cos(x);
            assert!((a - s).abs() < 1e-15 && (b - c).abs() < 1e-15, "{x}: {a} {b}");
        }
    }
}
