//! Scalar helpers and the monotone cubic curve of the prototype's `uniform-geo.js` (lines 7 to 33 and 44 to 49).
//!
//! Everything is `f64`, like the prototype, so a parity test can hold the Rust and the JavaScript values to rounding
//! noise. Operations are written in the prototype's order for the same reason.

use super::KitError;

/// A full turn, `2 * pi`.
pub const TAU: f64 = std::f64::consts::TAU;

/// `x` limited to `a..=b`. With `a > b` the result is `b`, as in the prototype.
pub fn clamp(x: f64, a: f64, b: f64) -> f64 {
    x.max(a).min(b)
}

/// Linear interpolation from `a` (at `t = 0`) to `b` (at `t = 1`), without clamping `t`.
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Smoothstep from 0 at `a` to 1 at `b`. Unlike `f64::clamp`-based versions it also works for a reversed range
/// (`a > b`), which gives a falling edge.
pub fn sstep(a: f64, b: f64, x: f64) -> f64 {
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The superellipse helper: `sign(c) * |c|^(2 / e)`. `e = 2` is a circle, a larger `e` squares the shape.
pub fn sup(c: f64, e: f64) -> f64 {
    // `f64::signum` returns 1 for zero; the prototype's `Math.sign(0)` is 0, which keeps `sup(0, e)` at exactly 0.
    if c == 0.0 { 0.0 } else { c.signum() * c.abs().powf(2.0 / e) }
}

/// `k / n` as a float, for grid indices. Indices are far below 2^53, so the conversion is exact.
#[allow(clippy::cast_precision_loss)] // grid indices and counts are tiny
pub(crate) fn fraction(k: usize, n: usize) -> f64 {
    k as f64 / n as f64
}

/// `n + 1` evenly spaced values from `a` to `b`, both included. With `n = 0` the result is just `[a]`.
#[allow(clippy::cast_precision_loss)] // see `fraction`
pub fn range(a: f64, b: f64, n: usize) -> Vec<f64> {
    if n == 0 {
        return vec![a];
    }
    (0..=n).map(|i| a + ((b - a) * i as f64) / n as f64).collect()
}

/// Concatenated `(a, b, n)` [`range`]s; where one range starts at the value the previous one ended on (within `1e-9`),
/// the duplicate is dropped so the joint appears once.
pub fn rows_of(specs: &[(f64, f64, usize)]) -> Vec<f64> {
    let mut out: Vec<f64> = Vec::new();
    for &(a, b, n) in specs {
        let mut r = range(a, b, n);
        if out.last().is_some_and(|last| (last - r[0]).abs() < 1e-9) {
            r.remove(0);
        }
        out.extend(r);
    }
    out
}

/// A monotone cubic Hermite interpolant (PCHIP, Fritsch-Carlson) through `(x, value)` knots: it never overshoots the
/// data, so a section radius curve through plausible radii cannot bulge between them. Outside the knots' range it holds
/// the end values. This is the prototype's `curve(pts)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pchip {
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// The slope at each knot.
    slopes: Vec<f64>,
}

impl Pchip {
    /// Builds the curve through `points`, in any order (they are sorted by x). One knot gives a constant.
    ///
    /// # Errors
    /// [`KitError`] for no knots, a NaN or infinite value, or two knots at the same x.
    pub fn new(points: &[(f64, f64)]) -> Result<Self, KitError> {
        if points.is_empty() {
            return Err(KitError::TooFew { what: "a curve's knots", need: 1, got: 0 });
        }
        if points.iter().any(|&(x, y)| !x.is_finite() || !y.is_finite()) {
            return Err(KitError::NonFinite { what: "a curve's knots" });
        }
        let mut sorted = points.to_vec();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Sorted ascending, so a knot that is not above its predecessor is equal to it.
        if let Some(pair) = sorted.windows(2).find(|w| w[1].0 <= w[0].0) {
            return Err(KitError::DuplicateKnot { x: pair[0].0 });
        }
        let (xs, ys): (Vec<f64>, Vec<f64>) = sorted.into_iter().unzip();
        let slopes = knot_slopes(&xs, &ys);
        Ok(Self { xs, ys, slopes })
    }

    /// The curve's value at `x`. NaN gives NaN.
    pub fn eval(&self, x: f64) -> f64 {
        let count = self.xs.len();
        if x.is_nan() {
            return f64::NAN;
        }
        if x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[count - 1] {
            return self.ys[count - 1];
        }
        // Here `xs[0] < x < xs[n - 1]`, so the first knot at or after `x` has an index of at least 1.
        let i = self.xs.partition_point(|&k| k < x) - 1;
        let width = self.xs[i + 1] - self.xs[i];
        let t = (x - self.xs[i]) / width;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * self.ys[i]
            + (t3 - 2.0 * t2 + t) * width * self.slopes[i]
            + (-2.0 * t3 + 3.0 * t2) * self.ys[i + 1]
            + (t3 - t2) * width * self.slopes[i + 1]
    }
}

/// Fritsch-Carlson slopes: the one-sided secant at the ends, zero at a local extremum, and the weighted harmonic mean
/// of the neighbouring secants elsewhere.
fn knot_slopes(xs: &[f64], ys: &[f64]) -> Vec<f64> {
    let n = xs.len();
    let mut d = vec![0.0; n];
    if n < 2 {
        return d;
    }
    let h: Vec<f64> = xs.windows(2).map(|w| w[1] - w[0]).collect();
    let secant: Vec<f64> = ys.windows(2).zip(&h).map(|(w, h)| (w[1] - w[0]) / h).collect();
    d[0] = secant[0];
    d[n - 1] = secant[n - 2];
    for i in 1..n - 1 {
        if secant[i - 1] * secant[i] <= 0.0 {
            d[i] = 0.0;
        } else {
            let w1 = 2.0 * h[i] + h[i - 1];
            let w2 = h[i] + 2.0 * h[i - 1];
            d[i] = (w1 + w2) / (w1 / secant[i - 1] + w2 / secant[i]);
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn scalar_helpers_match_the_prototype() {
        assert_eq!((clamp(5.0, 0.0, 1.0), clamp(-5.0, 0.0, 1.0), clamp(0.5, 0.0, 1.0)), (1.0, 0.0, 0.5));
        assert_eq!(
            clamp(0.5, 2.0, 1.0),
            1.0,
            "a reversed range gives the upper bound, like Math.min(b, Math.max(a, x))"
        );
        assert_eq!(lerp(2.0, 4.0, 0.25), 2.5);
        assert_eq!((sstep(0.0, 1.0, -1.0), sstep(0.0, 1.0, 2.0), sstep(0.0, 1.0, 0.5)), (0.0, 1.0, 0.5));
        assert_eq!((sstep(1.0, 0.0, 0.0), sstep(1.0, 0.0, 1.0)), (1.0, 0.0), "reversed ranges fall");
        assert!(close(sup(0.5, 2.0), 0.5) && close(sup(-0.5, 2.0), -0.5));
        assert!(close(sup(0.5, 4.0), 0.5_f64.powf(0.5)));
        assert_eq!(sup(0.0, 3.0), 0.0, "Math.sign(0) is 0");
    }

    #[test]
    fn range_includes_both_ends_and_rows_of_drops_the_shared_joint() {
        assert_eq!(range(0.0, 1.0, 4), [0.0, 0.25, 0.5, 0.75, 1.0]);
        assert_eq!(range(3.0, 9.0, 0), [3.0]);
        assert_eq!(rows_of(&[(0.0, 1.0, 2), (1.0, 3.0, 2), (3.5, 4.0, 1)]), [0.0, 0.5, 1.0, 2.0, 3.0, 3.5, 4.0]);
        assert_eq!(rows_of(&[]), Vec::<f64>::new());
    }

    #[test]
    fn the_curve_passes_through_its_knots_and_holds_the_ends() {
        let c = Pchip::new(&[(2.0, 4.0), (0.0, 0.0), (1.0, 1.0)]).unwrap();
        for (x, y) in [(0.0, 0.0), (1.0, 1.0), (2.0, 4.0)] {
            assert!(close(c.eval(x), y), "knot {x}");
        }
        assert_eq!((c.eval(-3.0), c.eval(9.0)), (0.0, 4.0));
        assert!(c.eval(f64::NAN).is_nan());
    }

    #[test]
    fn the_curve_never_overshoots_a_step() {
        let c = Pchip::new(&[(0.0, 0.0), (1.0, 0.0), (2.0, 1.0), (3.0, 1.0)]).unwrap();
        for k in 0..=300 {
            let y = c.eval(f64::from(k) / 100.0);
            assert!((-1e-12..=1.0 + 1e-12).contains(&y), "overshoot {y}");
        }
        let flat = Pchip::new(&[(0.0, 1.0), (1.0, 3.0), (2.0, 1.0)]).unwrap();
        assert!(flat.eval(1.0) >= flat.eval(0.9) && flat.eval(1.0) >= flat.eval(1.1), "a peak stays the maximum");
    }

    #[test]
    fn one_knot_is_a_constant_and_bad_knots_are_errors() {
        let c = Pchip::new(&[(1.0, 7.0)]).unwrap();
        assert_eq!((c.eval(-1.0), c.eval(1.0), c.eval(5.0)), (7.0, 7.0, 7.0));
        assert!(matches!(Pchip::new(&[]), Err(KitError::TooFew { .. })));
        assert_eq!(Pchip::new(&[(0.0, 0.0), (1.0, 1.0), (1.0, 2.0)]), Err(KitError::DuplicateKnot { x: 1.0 }));
        assert!(matches!(Pchip::new(&[(0.0, f64::NAN)]), Err(KitError::NonFinite { .. })));
        assert!(matches!(Pchip::new(&[(f64::INFINITY, 0.0)]), Err(KitError::NonFinite { .. })));
    }
}

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    use super::{Pchip, fraction};

    /// Knots from a start point and a walk of `(dx, dy)` steps. Each `dx` is at least 1e-3, so x is strictly increasing
    /// and no two knots share an x.
    fn walk(steps: impl Strategy<Value = (f64, f64)>) -> impl Strategy<Value = Vec<(f64, f64)>> {
        (-10.0..10.0_f64, -50.0..50.0_f64, prop::collection::vec(steps, 1..11)).prop_map(|(x0, y0, steps)| {
            let (mut x, mut y) = (x0, y0);
            let mut points = vec![(x, y)];
            for (dx, dy) in steps {
                x += dx;
                y += dy;
                points.push((x, y));
            }
            points
        })
    }

    /// Steps that never lower y, so the knots rise (or stay level).
    fn rising() -> impl Strategy<Value = (f64, f64)> {
        (1e-3..5.0_f64, 0.0..10.0_f64)
    }

    /// Steps that move y either way.
    fn any_direction() -> impl Strategy<Value = (f64, f64)> {
        (1e-3..5.0_f64, -100.0..100.0_f64)
    }

    /// Sixteen points in each interval between knots, and the last knot, in increasing x.
    fn dense_samples(points: &[(f64, f64)]) -> Vec<f64> {
        const PER_INTERVAL: usize = 16;
        let mut xs = Vec::new();
        for pair in points.windows(2) {
            let (a, b) = (pair[0].0, pair[1].0);
            xs.extend((0..PER_INTERVAL).map(|s| a + (b - a) * fraction(s, PER_INTERVAL)));
        }
        xs.extend(points.last().map(|p| p.0));
        xs
    }

    /// The range of the knots' y values, so a bound can scale its rounding tolerance with the data.
    fn y_range(points: &[(f64, f64)]) -> (f64, f64) {
        points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(_, y)| (lo.min(y), hi.max(y)))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// Monotone knots, rising or falling, give a curve that never turns back between them.
        #[test]
        fn monotone_knots_give_a_monotone_curve(rise in walk(rising()), falling in any::<bool>()) {
            let points: Vec<(f64, f64)> = rise.iter().map(|&(x, y)| (x, if falling { -y } else { y })).collect();
            let curve = Pchip::new(&points).unwrap();
            let (lo, hi) = y_range(&points);
            let tol = 1e-9 * (1.0 + lo.abs().max(hi.abs()));
            let values: Vec<f64> = dense_samples(&points).into_iter().map(|x| curve.eval(x)).collect();
            for pair in values.windows(2) {
                if falling {
                    prop_assert!(pair[1] <= pair[0] + tol, "rose from {} to {}", pair[0], pair[1]);
                } else {
                    prop_assert!(pair[1] >= pair[0] - tol, "fell from {} to {}", pair[0], pair[1]);
                }
            }
        }

        /// Whatever the knots, the curve stays inside the range of their values: PCHIP never overshoots.
        #[test]
        fn the_curve_never_overshoots_the_data(points in walk(any_direction())) {
            let curve = Pchip::new(&points).unwrap();
            let (lo, hi) = y_range(&points);
            let tol = 1e-9 * (1.0 + lo.abs().max(hi.abs()));
            for x in dense_samples(&points) {
                let v = curve.eval(x);
                prop_assert!(v >= lo - tol && v <= hi + tol, "{} outside [{}, {}] at x = {}", v, lo, hi, x);
            }
        }

        /// The curve passes through every knot exactly, not just approximately.
        #[test]
        #[allow(clippy::float_cmp)] // exact by construction: at a knot the Hermite basis is exactly 1 or 0, so only y survives
        fn the_curve_interpolates_its_knots(points in walk(any_direction())) {
            let curve = Pchip::new(&points).unwrap();
            for &(x, y) in &points {
                prop_assert_eq!(curve.eval(x), y);
            }
        }
    }
}
