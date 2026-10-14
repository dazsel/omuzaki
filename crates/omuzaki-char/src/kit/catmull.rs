//! Catmull-Rom arc-length points, exactly as three.js computes them (port of `hair.js` `catmull`, lines 22 to 26).
//!
//! The prototype calls `new THREE.CatmullRomCurve3(pts, false, 'centripetal', 0.5).getSpacedPoints(n - 1)`. Matching
//! it to a millionth of a metre needs three's algorithm, not just any Catmull-Rom spline, so this module reproduces
//! three@0.170.0 step by step:
//!
//! 1. `getPoint(t)` (`CatmullRom::point`): `t` in `0..=1` is spread evenly over the segments, not by length. A segment
//!    is the centripetal (non-uniform) Catmull-Rom cubic through its two points, with the neighbours as tangent
//!    guides and the missing neighbour at either end mirrored from the nearest point.
//! 2. `getLengths()` (`CatmullRom::arc_lengths`): the curve is sampled at 200 equal steps of `t` and the chord lengths
//!    are summed, so the arc length is a 200-chord approximation and spaced points are only as even as that is.
//! 3. `getUtoTmapping(u)` (`t_at`): the wanted length `u * total` is located among the sums and `t` is
//!    interpolated linearly inside that step.
//!
//! The quirks that make a result differ from a textbook spline are kept on purpose: the repeated-point guard
//! (`step_lengths`), and the shared scratch vector that makes a two-point curve use its mirrored end point for both
//! tangent guides (`CatmullRom::guides`).
//!
//! This is a port of MIT-licensed three.js code: the notice is in `NOTICES.md` beside this file, and the register has the row.

use omuzaki_core::glam::DVec3;

use super::KitError;
use super::curve::fraction;
use super::resample::vec_for;

/// three's `Curve.arcLengthDivisions` default: the curve is measured in this many chords.
const ARC_LENGTH_DIVISIONS: usize = 200;

/// A parameter step (the square root of the distance between two points) below this means a repeated point: a
/// distance below `1e-8`.
const REPEATED_POINT: f64 = 1e-4;

/// `n` points spaced evenly by arc length along the centripetal Catmull-Rom curve through `pts`, the first at
/// `pts[0]` and the last at the final point. This is the prototype's `catmull(pts, n)`.
///
/// Consecutive repeated points are allowed, as in three, whose repeated-point guard keeps the tangents finite.
///
/// # Errors
/// - [`KitError::TooFew`] for fewer than 2 points or `n < 2`.
/// - [`KitError::NonFinite`] for a NaN or infinite coordinate, or if the curve's length (or a result) is not finite.
/// - [`KitError::Degenerate`] if every point coincides, so the curve has no length to space points along.
/// - [`KitError::Overflow`] if `n` points cannot be allocated.
pub fn spaced_points(pts: &[DVec3], n: usize) -> Result<Vec<DVec3>, KitError> {
    if pts.len() < 2 {
        return Err(KitError::TooFew { what: "points of a Catmull-Rom curve", need: 2, got: pts.len() });
    }
    if n < 2 {
        return Err(KitError::TooFew { what: "spaced points", need: 2, got: n });
    }
    if pts.iter().any(|p| !p.is_finite()) {
        return Err(KitError::NonFinite { what: "a Catmull-Rom curve's points" });
    }
    let curve = CatmullRom { points: pts };
    let lengths = curve.arc_lengths();
    let total = lengths[ARC_LENGTH_DIVISIONS];
    if !total.is_finite() {
        return Err(KitError::NonFinite { what: "a Catmull-Rom curve's length" });
    }
    if total <= 0.0 {
        return Err(KitError::Degenerate { what: "a Catmull-Rom curve of zero length" });
    }
    let mut out = vec_for(n)?;
    for d in 0..n {
        let target = fraction(d, n - 1) * total;
        out.push(curve.point(t_at(&lengths, target)));
    }
    if out.iter().any(|p| !p.is_finite()) {
        return Err(KitError::NonFinite { what: "a spaced point" });
    }
    Ok(out)
}

/// The cumulative chord lengths of a curve at `t = k / 200`: entry `k` is the length up to that step.
type ArcLengths = [f64; ARC_LENGTH_DIVISIONS + 1];

/// three's `getUtoTmapping`: the `t` at which the curve has run `target` of its length, `target` being in
/// `0..=lengths[200]`. The chord holding `target` is found by binary search and `t` is interpolated linearly inside it.
///
/// three searches for the last sum below the target and stops early on an exact match; `partition_point` finds the
/// same sum, and for an exact match the first of equal sums. Equal sums are chords of zero length, so every choice is
/// the same point of the curve.
fn t_at(lengths: &ArcLengths, target: f64) -> f64 {
    let first_not_below = lengths.partition_point(|&len| len < target);
    let Some(&after) = lengths.get(first_not_below) else { return 1.0 };
    #[allow(clippy::float_cmp)] // an exact hit is a sum equal to the target bit for bit, as in three
    if after == target {
        return fraction(first_not_below, ARC_LENGTH_DIVISIONS);
    }
    // Only a negative target has no sum below it, and none is asked for; 0 keeps the answer defined anyway.
    let Some(before) = first_not_below.checked_sub(1) else { return 0.0 };
    let length_before = lengths[before];
    let segment_fraction = (target - length_before) / (after - length_before);
    (count_f64(before) + segment_fraction) / count_f64(ARC_LENGTH_DIVISIONS)
}

/// A count as a float; counts here are a few hundred at most, so the conversion is exact.
#[allow(clippy::cast_precision_loss)]
fn count_f64(k: usize) -> f64 {
    k as f64
}

/// three's `CatmullRomCurve3(points, closed = false, 'centripetal')`, borrowing at least two finite points.
struct CatmullRom<'a> {
    points: &'a [DVec3],
}

impl CatmullRom<'_> {
    /// The 200-chord cumulative lengths (`getLengths`).
    fn arc_lengths(&self) -> ArcLengths {
        let mut lengths = [0.0; ARC_LENGTH_DIVISIONS + 1];
        let mut last = self.point(0.0);
        let mut sum = 0.0;
        for (p, slot) in lengths.iter_mut().enumerate().skip(1) {
            let current = self.point(fraction(p, ARC_LENGTH_DIVISIONS));
            sum += current.distance(last);
            *slot = sum;
            last = current;
        }
        lengths
    }

    /// The four points whose cubic covers segment `segment` (`segment + 1` is its far end): the neighbour before it,
    /// both ends, and the neighbour after it. At either end of the curve the missing neighbour is the end point
    /// mirrored across its nearest neighbour.
    ///
    /// With exactly two points both neighbours are missing. three keeps its mirrored points in one shared scratch
    /// vector, so the second overwrites the first: the "before" guide ends up equal to the "after" guide. That is
    /// reproduced here, because the prototype's result depends on it.
    fn guides(&self, segment: usize) -> [DVec3; 4] {
        let pts = self.points;
        let count = pts.len();
        // three wraps its indices with `%` even on an open curve; only a `t` past 1 can reach the wrap.
        let at = |k: usize| pts[k % count];
        let after =
            if segment + 2 < count { at(segment + 2) } else { (pts[count - 1] - pts[count - 2]) + pts[count - 1] };
        let before = if segment > 0 {
            at(segment - 1)
        } else if segment + 2 < count {
            (pts[0] - pts[1]) + pts[0]
        } else {
            after // the shared scratch vector
        };
        [before, at(segment), at(segment + 1), after]
    }

    /// The point at curve parameter `t` in `0..=1` (`getPoint`).
    fn point(&self, t: f64) -> DVec3 {
        let count = self.points.len();
        let scaled = count_f64(count - 1) * t;
        let floor = scaled.floor();
        let mut weight = scaled - floor;
        // `0 <= scaled <= count - 1`, and an `as` cast saturates anyway.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut segment = floor as usize;
        #[allow(clippy::float_cmp)] // exactly the end of the curve, as in three
        if weight == 0.0 && segment == count - 1 {
            segment = count - 2;
            weight = 1.0;
        }
        let guides = self.guides(segment);
        let steps = step_lengths(guides);
        let [g0, g1, g2, g3] = guides.map(|v| v.to_array());
        DVec3::from_array(std::array::from_fn(|k| cubic([g0[k], g1[k], g2[k], g3[k]], steps, weight)))
    }
}

/// The centripetal parameter steps between the four guides: each is the fourth root of the squared distance, i.e. the
/// square root of the distance. three's repeated-point guard follows: a middle step below `1e-4` becomes 1, and an
/// outer step below it copies the middle one. The order matters (the outer steps see the middle step already fixed).
fn step_lengths([p0, p1, p2, p3]: [DVec3; 4]) -> [f64; 3] {
    let mut dt0 = p0.distance_squared(p1).powf(0.25);
    let mut dt1 = p1.distance_squared(p2).powf(0.25);
    let mut dt2 = p2.distance_squared(p3).powf(0.25);
    if dt1 < REPEATED_POINT {
        dt1 = 1.0;
    }
    if dt0 < REPEATED_POINT {
        dt0 = dt1;
    }
    if dt2 < REPEATED_POINT {
        dt2 = dt1;
    }
    [dt0, dt1, dt2]
}

/// One coordinate of a non-uniform Catmull-Rom segment at `weight` in `0..=1` (three's `initNonuniformCatmullRom`
/// followed by `calc`): the cubic Hermite through `x1` and `x2` whose tangents are fitted over the parameter steps
/// `dt0`, `dt1` and `dt2` and rescaled to the segment.
fn cubic([x0, x1, x2, x3]: [f64; 4], [dt0, dt1, dt2]: [f64; 3], weight: f64) -> f64 {
    let mut tangent1 = (x1 - x0) / dt0 - (x2 - x0) / (dt0 + dt1) + (x2 - x1) / dt1;
    let mut tangent2 = (x2 - x1) / dt1 - (x3 - x1) / (dt1 + dt2) + (x3 - x2) / dt2;
    tangent1 *= dt1;
    tangent2 *= dt1;
    let c2 = -3.0 * x1 + 3.0 * x2 - 2.0 * tangent1 - tangent2;
    let c3 = 2.0 * x1 - 2.0 * x2 + tangent1 + tangent2;
    let squared = weight * weight;
    let cubed = squared * weight;
    x1 + tangent1 * weight + c2 * squared + c3 * cubed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(count: usize) -> Vec<DVec3> {
        (0..count).map(|i| DVec3::new(fraction(i, count - 1), 0.0, 0.0)).collect()
    }

    #[test]
    fn spaced_points_start_and_end_at_the_curve_ends() {
        let pts = [
            DVec3::new(0.0, 1.0, 0.0),
            DVec3::new(0.1, 0.7, 0.2),
            DVec3::new(0.0, 0.4, 0.1),
            DVec3::new(-0.1, 0.0, 0.0),
        ];
        let spaced = spaced_points(&pts, 9).unwrap();
        assert_eq!(spaced.len(), 9);
        assert!((spaced[0] - pts[0]).length() < 1e-12);
        assert!((spaced[8] - pts[3]).length() < 1e-12);
    }

    #[test]
    fn a_straight_line_is_spaced_evenly() {
        let spaced = spaced_points(&line(4), 11).unwrap();
        for (k, p) in spaced.iter().enumerate() {
            assert!((p.x - fraction(k, 10)).abs() < 1e-4, "point {k} at {}", p.x);
            assert!(p.y.abs() < 1e-12 && p.z.abs() < 1e-12);
        }
    }

    #[test]
    fn two_points_make_a_finite_curve_that_ends_on_both() {
        let pts = [DVec3::ZERO, DVec3::new(0.1, 0.2, -0.05)];
        let spaced = spaced_points(&pts, 5).unwrap();
        assert!((spaced[0] - pts[0]).length() < 1e-12);
        assert!((spaced[4] - pts[1]).length() < 1e-12);
    }

    #[test]
    fn a_two_point_curve_uses_its_far_mirror_for_both_guides_like_three() {
        let curve = CatmullRom { points: &[DVec3::ZERO, DVec3::X] };
        let [before, start, end, after] = curve.guides(0);
        assert_eq!((start, end), (DVec3::ZERO, DVec3::X));
        assert_eq!(after, DVec3::new(2.0, 0.0, 0.0), "the far end mirrored across the near one");
        assert_eq!(before, after, "three's shared scratch vector");
        let curve = CatmullRom { points: &[DVec3::ZERO, DVec3::X, DVec3::Y] };
        assert_eq!(
            curve.guides(0)[0],
            DVec3::new(-1.0, 0.0, 0.0),
            "with three points the first guide is its own mirror"
        );
    }

    #[test]
    fn repeated_points_keep_the_tangents_finite() {
        let p = DVec3::new(0.1, 0.05, 0.0);
        let pts = [DVec3::ZERO, p, p, DVec3::new(0.2, 0.0, 0.05), DVec3::new(0.3, 0.1, 0.05)];
        let spaced = spaced_points(&pts, 20).unwrap();
        assert!(spaced.iter().all(|s| s.is_finite()));
        let ends = [DVec3::ZERO, DVec3::ZERO, DVec3::X];
        assert!(spaced_points(&ends, 8).unwrap().iter().all(|s| s.is_finite()));
    }

    #[test]
    fn the_repeated_point_guard_replaces_short_steps_in_three_s_order() {
        let (a, c) = (DVec3::ZERO, DVec3::new(16.0, 0.0, 0.0)); // the square root of 16 is 4
        assert_eq!(
            step_lengths([a, a, a, a]),
            [1.0, 1.0, 1.0],
            "every step repeated: the middle becomes 1, the rest copy it"
        );
        assert_eq!(step_lengths([a, a, a, c]), [1.0, 1.0, 4.0], "the first step copies the already fixed middle step");
        assert_eq!(step_lengths([a, a, c, c]), [4.0, 4.0, 4.0], "short outer steps copy a real middle step");
        assert_eq!(step_lengths([c, a, a, a]), [4.0, 1.0, 1.0]);
        let almost = DVec3::new(1e-9, 0.0, 0.0); // a distance below 1e-8 is a fourth root below 1e-4
        assert_eq!(step_lengths([a, almost, a, a]), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn bad_input_is_a_typed_error_never_nan_or_a_panic() {
        let two = [DVec3::ZERO, DVec3::X];
        assert!(matches!(spaced_points(&[], 4), Err(KitError::TooFew { need: 2, got: 0, .. })));
        assert!(matches!(spaced_points(&[DVec3::X], 4), Err(KitError::TooFew { need: 2, got: 1, .. })));
        assert!(matches!(spaced_points(&two, 0), Err(KitError::TooFew { need: 2, got: 0, .. })));
        assert!(matches!(spaced_points(&two, 1), Err(KitError::TooFew { need: 2, got: 1, .. })));
        assert!(matches!(
            spaced_points(&[DVec3::ZERO, DVec3::new(f64::NAN, 0.0, 0.0)], 4),
            Err(KitError::NonFinite { .. })
        ));
        assert!(matches!(
            spaced_points(&[DVec3::ZERO, DVec3::new(0.0, f64::INFINITY, 0.0)], 4),
            Err(KitError::NonFinite { .. })
        ));
        let same = DVec3::new(0.3, 0.2, 0.1);
        assert!(matches!(spaced_points(&[same, same, same], 4), Err(KitError::Degenerate { .. })));
        let huge = [DVec3::splat(-1e308), DVec3::splat(1e308)];
        assert!(matches!(spaced_points(&huge, 4), Err(KitError::NonFinite { .. })), "length overflows");
        assert!(matches!(spaced_points(&two, usize::MAX), Err(KitError::Overflow { .. })));
    }

    #[test]
    fn the_length_search_finds_the_chord_and_interpolates() {
        let mut lengths = [0.0; ARC_LENGTH_DIVISIONS + 1];
        for (k, len) in lengths.iter_mut().enumerate() {
            *len = fraction(k, 1) * 2.0; // a uniform curve of length 400
        }
        assert_eq!(t_at(&lengths, 0.0), 0.0);
        assert_eq!(t_at(&lengths, 400.0), 1.0);
        assert!((t_at(&lengths, 100.0) - 0.25).abs() < 1e-15);
        assert!((t_at(&lengths, 101.0) - 0.2525).abs() < 1e-15, "inside a chord");
    }
}
