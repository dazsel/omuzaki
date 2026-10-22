//! Linear prediction: the autocorrelation of a windowed grain, Levinson-Durbin, and the line spectral frequencies the
//! formant warp moves. Everything works in fixed arrays of the order [`ORDER`], so nothing allocates.

use std::f64::consts::PI;

/// The prediction order: 32 poles at 48 kHz. Measured on the synthetic voice: at 24 the envelope of a 120 Hz voice
/// leans on its harmonics and puts F1 5.6% low after a +5 semitone shift; at 32 every shift keeps F1 to F3 within
/// 3.5% of the same vowel said at the target pitch; 18 cannot hold /a/'s F1 and F2 apart once warped.
pub const ORDER: usize = 32;

/// The coefficients `[1, a1, ..., ap]` of `A(z) = 1 + Σ a_i z^-i`.
pub type Poly = [f64; ORDER + 1];

/// The identity predictor, `A(z) = 1`.
pub const IDENTITY: Poly = {
    let mut a = [0.0; ORDER + 1];
    a[0] = 1.0;
    a
};

/// The line spectral frequencies of an `A(z)`, in radians, ascending in `(0, π)`.
pub type Lsf = [f64; ORDER];

/// Levinson-Durbin on the autocorrelation `r[0..=ORDER]`. A silent grain (or one the recursion cannot make stable)
/// gives the identity.
#[must_use]
pub fn levinson(r: &[f64; ORDER + 1]) -> Poly {
    let mut a = IDENTITY;
    if r[0] <= 1e-12 {
        return a;
    }
    let mut err = r[0];
    let mut prev = IDENTITY;
    for i in 1..=ORDER {
        let mut acc = r[i];
        for j in 1..i {
            acc += prev[j] * r[i - j];
        }
        let k = -acc / err;
        if !k.is_finite() || k.abs() >= 1.0 {
            return prev;
        }
        a[i] = k;
        for j in 1..i {
            a[j] = prev[j] + k * prev[i - j];
        }
        err *= 1.0 - k * k;
        prev = a;
        if err <= r[0] * 1e-12 {
            break;
        }
    }
    a
}

/// `A(z)` evaluated as `|A(e^jω)|²`, for the envelope a test reads formants from.
#[must_use]
pub fn power(a: &Poly, w: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (k, c) in a.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)] // a polynomial index
        let (s, co) = (w * k as f64).sin_cos();
        re += c * co;
        im -= c * s;
    }
    re * re + im * im
}

/// The energy of the impulse response of `1 / A(z)`, exactly: `1 / Π (1 − k_i²)` over the reflection coefficients
/// `k_i` the step-down recursion finds. `None` when `A` is not minimum phase (some `|k_i| ≥ 1`).
#[must_use]
pub fn power_gain(a: &Poly) -> Option<f64> {
    let mut a = *a;
    let mut gain = 1.0;
    for m in (1..=ORDER).rev() {
        let k = a[m];
        let rest = 1.0 - k * k;
        if rest <= 1e-12 || !rest.is_finite() {
            return None;
        }
        gain /= rest;
        let previous = a;
        for i in 1..m {
            a[i] = (previous[i] - k * previous[m - i]) / rest;
        }
        a[m] = 0.0;
    }
    Some(gain)
}

const HALF: usize = ORDER / 2;

/// The two symmetric halves of `A(z)` with their trivial roots divided out: `P(z) / (1 + z^-1)` and
/// `Q(z) / (1 − z^-1)`, as the coefficients of `Σ c_k cos((m − k)ω)`.
fn halves(a: &Poly) -> ([f64; HALF + 1], [f64; HALF + 1]) {
    let (mut p, mut q) = ([0.0; HALF + 1], [0.0; HALF + 1]);
    p[0] = 1.0;
    q[0] = 1.0;
    for k in 1..=HALF {
        p[k] = a[k] + a[ORDER + 1 - k] - p[k - 1];
        q[k] = a[k] - a[ORDER + 1 - k] + q[k - 1];
    }
    (p, q)
}

/// `Σ_{k<m} c_k cos((m − k) ω) + c_m / 2` at `x = cos ω`, by Clenshaw's recurrence on the Chebyshev polynomials.
fn cheb(c: &[f64; HALF + 1], x: f64) -> f64 {
    let (mut b1, mut b2) = (0.0, 0.0);
    // T_j(x) multiplies c_{m-j}; run j from m down to 1.
    for j in (1..=HALF).rev() {
        let b0 = 2.0 * x * b1 - b2 + c[HALF - j];
        b2 = b1;
        b1 = b0;
    }
    x * b1 - b2 + c[HALF] / 2.0
}

/// How finely the unit circle is searched for sign changes: 50 Hz steps at 48 kHz, as fine as the closest pair the
/// formant warp allows.
const GRID: usize = 480;

/// The line spectral frequencies of `a`, or `None` when the roots could not all be found (an unstable or degenerate
/// predictor); the caller then keeps the previous grain's.
#[must_use]
pub fn to_lsf(a: &Poly) -> Option<Lsf> {
    let (p, q) = halves(a);
    let grid = grid();
    let mut out = [0.0; ORDER];
    let mut found = 0;
    let (mut fp, mut fq) = (cheb(&p, 1.0), cheb(&q, 1.0));
    for i in 1..=GRID {
        let x = grid[i];
        let (np, nq) = (cheb(&p, x), cheb(&q, x));
        for (poly, before, now) in [(&p, fp, np), (&q, fq, nq)] {
            if before * now < 0.0 || (now == 0.0 && before != 0.0) {
                if found == ORDER {
                    return None;
                }
                // The root in x = cos ω between the grid points (cos falls monotonically on [0, π]), by the Illinois
                // variant of regula falsi: a few polynomial evaluations and no trigonometry per step.
                let (mut a, mut b, mut fa, mut fb) = (grid[i - 1], x, before, now);
                let mut root = b;
                for _ in 0..48 {
                    let next = (a * fb - b * fa) / (fb - fa);
                    if !next.is_finite() || (next - root).abs() < 1e-14 {
                        break;
                    }
                    root = next;
                    let f = cheb(poly, root);
                    if f == 0.0 {
                        break;
                    }
                    if f * fb < 0.0 {
                        (a, fa) = (b, fb);
                    } else {
                        fa *= 0.5;
                    }
                    (b, fb) = (root, f);
                }
                out[found] = root.clamp(-1.0, 1.0).acos();
                found += 1;
            }
        }
        fp = np;
        fq = nq;
    }
    if found != ORDER {
        return None;
    }
    out.sort_by(f64::total_cmp);
    Some(out)
}

/// `cos(π i / GRID)` for `i` in `0..=GRID`, computed once.
fn grid() -> &'static [f64; GRID + 1] {
    static GRID_COS: std::sync::OnceLock<[f64; GRID + 1]> = std::sync::OnceLock::new();
    GRID_COS.get_or_init(|| {
        #[allow(clippy::cast_precision_loss)] // a grid index
        std::array::from_fn(|i| (PI * i as f64 / GRID as f64).cos())
    })
}

/// The predictor whose line spectral frequencies are `lsf` (ascending, alternating between the two halves, the lowest
/// belonging to `P`).
#[must_use]
pub fn from_lsf(lsf: &Lsf) -> Poly {
    // P'(z) = Π (1 − 2 cos ω z^-1 + z^-2) over the odd LSFs; Q'(z) likewise over the even ones.
    let mut p = [0.0; ORDER + 2];
    let mut q = [0.0; ORDER + 2];
    p[0] = 1.0;
    q[0] = 1.0;
    let (mut lp, mut lq) = (0, 0);
    for (i, w) in lsf.iter().enumerate() {
        let c = -2.0 * w.cos();
        let (poly, len) = if i % 2 == 0 { (&mut p, &mut lp) } else { (&mut q, &mut lq) };
        // Multiply by (1 + c z^-1 + z^-2), in place, from the top.
        for k in (0..=*len + 2).rev() {
            let mut v = poly[k];
            if k >= 1 {
                v += c * poly[k - 1];
            }
            if k >= 2 {
                v += poly[k - 2];
            }
            poly[k] = v;
        }
        *len += 2;
    }
    // P(z) = P'(z)(1 + z^-1), Q(z) = Q'(z)(1 − z^-1), A = (P + Q) / 2.
    let mut a = IDENTITY;
    for k in 1..=ORDER {
        let pk = p[k] + p[k - 1];
        let qk = q[k] - q[k - 1];
        a[k] = 0.5 * (pk + qk);
    }
    a
}

/// Where the warp stops being a plain scaling, as a fraction of π in the warped spectrum (14.4 kHz at 48 kHz).
pub const KNEE: f64 = 0.6;

/// Warps `lsf` by `beta`, keeping them `min_gap` apart and under `ceiling`.
///
/// Up to the knee every frequency moves by exactly `beta` (so every formant of a voice does); above it the rest of the
/// band is mapped linearly onto what is left up to π. A plain `clamp(β ω)` would pile the top LSFs against the
/// ceiling at the minimum gap for β > 1, which makes a row of needle-sharp resonances near Nyquist and rings.
#[must_use]
pub fn warp(lsf: &Lsf, beta: f64, min_gap: f64, ceiling: f64) -> Lsf {
    let knee = KNEE * PI;
    let from = knee / beta;
    let mut out = lsf.map(|w| {
        let warped = if w <= from { w * beta } else { knee + (w - from) * (PI - knee) / (PI - from) };
        warped.clamp(min_gap, ceiling)
    });
    for i in 1..ORDER {
        out[i] = out[i].max(out[i - 1] + min_gap);
    }
    // From the top down, so the ceiling holds and the gaps still do.
    out[ORDER - 1] = out[ORDER - 1].min(ceiling);
    for i in (0..ORDER - 1).rev() {
        out[i] = out[i].min(out[i + 1] - min_gap);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resonant() -> Poly {
        // Two resonances (700 and 1,200 Hz) and a gentle tilt, as a predictor.
        let mut r = [0.0; ORDER + 1];
        for (k, v) in r.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let t = k as f64;
            *v = (-0.02 * t).exp() * (2.0 * PI * 700.0 / 48_000.0 * t).cos()
                + 0.6 * (-0.03 * t).exp() * (2.0 * PI * 1200.0 / 48_000.0 * t).cos()
                + if k == 0 { 0.05 } else { 0.0 };
        }
        levinson(&r)
    }

    #[test]
    fn lsf_round_trips_and_is_ordered() {
        let a = resonant();
        let lsf = to_lsf(&a).expect("a stable predictor has all its LSFs");
        assert!(lsf.windows(2).all(|w| w[0] < w[1]) && lsf[0] > 0.0 && lsf[ORDER - 1] < PI);
        let back = from_lsf(&lsf);
        let worst = a.iter().zip(&back).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-7, "{worst}");
    }

    #[test]
    fn a_warp_of_one_changes_nothing_and_keeps_gaps() {
        let lsf = to_lsf(&resonant()).unwrap();
        assert_eq!(warp(&lsf, 1.0, 0.0, PI), lsf);
        let up = warp(&lsf, 1.4, 0.01, 0.98 * PI);
        assert!(up.windows(2).all(|w| w[1] - w[0] >= 0.01 - 1e-12) && up[ORDER - 1] <= 0.98 * PI);
    }
}
