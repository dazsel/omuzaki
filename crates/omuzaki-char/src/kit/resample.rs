//! Polyline resampling by arc length (port of `uniform-geo.js` `resample`, lines 275 to 283).

use omuzaki_core::glam::DVec3;

use super::KitError;
use super::curve::{clamp, fraction};

/// The shortest segment `resample` divides by, so a repeated point (a zero-length segment) cannot divide by zero.
const MIN_SEGMENT: f64 = 1e-9;

/// An empty vector with room for `count` points, or [`KitError::Overflow`] when that many cannot be allocated, so a
/// huge count is an error instead of a panic or an aborted process.
pub(super) fn vec_for(count: usize) -> Result<Vec<DVec3>, KitError> {
    let mut points = Vec::new();
    points.try_reserve_exact(count).map_err(|_| KitError::Overflow { what: "points of a resampled curve" })?;
    Ok(points)
}

/// `n + 1` points evenly spaced by arc length along the polyline through `pts`, the first at `pts[0]` and the last at
/// the final point. Each lies on the segment that holds its distance, interpolated linearly (the prototype's
/// `resample(pts, n)`).
///
/// A repeated point is fine: its zero-length segment never holds a sample.
///
/// # Errors
/// - [`KitError::TooFew`] for fewer than 2 points or `n == 0`.
/// - [`KitError::NonFinite`] for a NaN or infinite coordinate, or a polyline whose length overflows.
/// - [`KitError::Degenerate`] if every point coincides, so there is no length to divide.
/// - [`KitError::Overflow`] if `n + 1` points cannot be allocated.
pub fn resample(pts: &[DVec3], n: usize) -> Result<Vec<DVec3>, KitError> {
    if pts.len() < 2 {
        return Err(KitError::TooFew { what: "points of a polyline", need: 2, got: pts.len() });
    }
    if n == 0 {
        return Err(KitError::TooFew { what: "segments to resample into", need: 1, got: 0 });
    }
    if pts.iter().any(|p| !p.is_finite()) {
        return Err(KitError::NonFinite { what: "a polyline's points" });
    }
    // `along[i]` is the distance from the first point to point `i`.
    let mut along = Vec::with_capacity(pts.len());
    along.push(0.0);
    let mut run = 0.0;
    for pair in pts.windows(2) {
        run += pair[1].distance(pair[0]);
        along.push(run);
    }
    if !run.is_finite() {
        return Err(KitError::NonFinite { what: "a polyline's length" });
    }
    if run <= 0.0 {
        return Err(KitError::Degenerate { what: "a polyline of zero length" });
    }
    let count = n.checked_add(1).ok_or(KitError::Overflow { what: "points of a resampled curve" })?;
    let mut out = vec_for(count)?;
    // The distances only grow, so the segment index never has to go back: this is the prototype's rescan from the
    // start for each sample, made linear.
    let mut i = 1;
    for k in 0..count {
        let d = run * fraction(k, n);
        while i < along.len() - 1 && along[i] < d {
            i += 1;
        }
        let t = (d - along[i - 1]) / (along[i] - along[i - 1]).max(MIN_SEGMENT);
        let (from, to) = (pts[i - 1], pts[i]);
        out.push(from + (to - from) * clamp(t, 0.0, 1.0));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: DVec3, b: DVec3) -> bool {
        (a - b).length() < 1e-12
    }

    #[test]
    fn samples_are_evenly_spaced_along_a_bent_polyline() {
        // An L: 1 along X, then 1 along Y, so 5 samples (n = 4) sit every half unit.
        let pts = [DVec3::ZERO, DVec3::X, DVec3::new(1.0, 1.0, 0.0)];
        let s = resample(&pts, 4).unwrap();
        let want =
            [DVec3::ZERO, DVec3::new(0.5, 0.0, 0.0), DVec3::X, DVec3::new(1.0, 0.5, 0.0), DVec3::new(1.0, 1.0, 0.0)];
        assert_eq!(s.len(), 5);
        for (got, want) in s.iter().zip(want) {
            assert!(close(*got, want), "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn the_ends_are_kept_and_more_samples_than_points_work() {
        let pts = [DVec3::new(0.0, 1.0, 2.0), DVec3::new(3.0, 1.0, 2.0)];
        let s = resample(&pts, 30).unwrap();
        assert_eq!(s.len(), 31);
        assert!(close(s[0], pts[0]) && close(s[30], pts[1]));
        assert!(s.windows(2).all(|w| ((w[1] - w[0]).length() - 0.1).abs() < 1e-12));
    }

    #[test]
    fn a_repeated_point_is_skipped_over() {
        let pts = [DVec3::ZERO, DVec3::X, DVec3::X, DVec3::new(2.0, 0.0, 0.0)];
        let s = resample(&pts, 4).unwrap();
        assert!(s.iter().enumerate().all(|(k, p)| close(*p, DVec3::new(0.5 * fraction(k, 1), 0.0, 0.0))));
    }

    #[test]
    fn bad_input_is_a_typed_error_never_nan_or_a_panic() {
        let two = [DVec3::ZERO, DVec3::X];
        assert!(matches!(resample(&[], 4), Err(KitError::TooFew { need: 2, got: 0, .. })));
        assert!(matches!(resample(&[DVec3::X], 4), Err(KitError::TooFew { need: 2, got: 1, .. })));
        assert!(matches!(resample(&two, 0), Err(KitError::TooFew { need: 1, got: 0, .. })));
        assert!(matches!(resample(&[DVec3::ZERO, DVec3::new(f64::NAN, 0.0, 0.0)], 4), Err(KitError::NonFinite { .. })));
        assert!(matches!(
            resample(&[DVec3::ZERO, DVec3::new(0.0, f64::INFINITY, 0.0)], 4),
            Err(KitError::NonFinite { .. })
        ));
        assert!(matches!(resample(&[DVec3::Y, DVec3::Y], 4), Err(KitError::Degenerate { .. })));
        assert!(matches!(resample(&[DVec3::splat(-1e308), DVec3::splat(1e308)], 4), Err(KitError::NonFinite { .. })));
        assert!(matches!(resample(&two, usize::MAX), Err(KitError::Overflow { .. })));
        assert!(
            matches!(resample(&two, usize::MAX - 1), Err(KitError::Overflow { .. })),
            "n + 1 points cannot be allocated"
        );
    }
}
