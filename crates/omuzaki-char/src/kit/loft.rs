//! Grids from a loft around the Y axis or a lathe around any axis (port of `uniform-geo.js` `latheAxis`, lines 97 to
//! 108, and `loftY`, lines 211 to 215).

use omuzaki_core::glam::DVec3;

use super::KitError;
use super::curve::{TAU, fraction};
use super::grid::Grid;

/// The rows of a loft around the Y axis: one row per height in `ys`, and `seg + 1` points around it. `shape(y, phi, i, j)`
/// gives the point at height `y` and angle `phi` (radians; 0 is the front, +Z, and `pi / 2` is +X, the character's
/// left), for row `i` and column `j`. The last column has `phi = 2 pi`, so a closed loft wraps with `wrap_j`.
///
/// # Errors
/// [`KitError`] for fewer than 2 heights or `seg < 1`, `seg + 1` or the point count overflowing `usize`, or a non-finite
/// point; `shape` is not called for a bad size.
pub fn loft_y(
    ys: &[f64],
    seg: usize,
    mut shape: impl FnMut(f64, f64, usize, usize) -> DVec3,
) -> Result<Grid, KitError> {
    Grid::from_fn(ys.len(), columns_of(seg)?, |i, j| shape(ys[i], fraction(j, seg) * TAU, i, j))
}

/// The columns of a ring of `seg` segments: `seg + 1` points, the last repeating the first. Checked, so a huge `seg`
/// is an error rather than an overflow.
fn columns_of(seg: usize) -> Result<usize, KitError> {
    seg.checked_add(1).ok_or(KitError::Overflow { what: "columns of a ring (segments + 1)" })
}

/// A lathe around the axis through `origin` in direction `axis`: for each `(u, r)` of `profile` a ring of radius `r`
/// at distance `u` along the axis, with `seg + 1` points (the last repeats the first). The ring starts in a direction
/// perpendicular to the axis (chosen from world Y, or X when the axis is nearly vertical) and turns right-handed about
/// the axis, so a profile whose `r` falls to 0 at an end gives a pole.
///
/// # Errors
/// [`KitError::Degenerate`] for a zero-length or non-finite axis, [`KitError::Overflow`] for a `seg` too large to count;
/// otherwise as [`Grid::new`].
pub fn lathe_axis(origin: DVec3, axis: DVec3, profile: &[(f64, f64)], seg: usize) -> Result<Grid, KitError> {
    let along = axis.normalize_or_zero();
    if along == DVec3::ZERO {
        return Err(KitError::Degenerate { what: "a lathe axis" });
    }
    let columns = columns_of(seg)?;
    let reference = if along.y.abs() < 0.9 { DVec3::Y } else { DVec3::X };
    let start = (reference + along * -reference.dot(along)).normalize();
    let turn = along.cross(start).normalize();
    Grid::from_fn(profile.len(), columns, |i, j| {
        let (u, r) = profile[i];
        let phi = fraction(j, seg) * TAU;
        origin + along * u + start * (phi.cos() * r) + turn * (phi.sin() * r)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::curve::range;

    #[test]
    fn a_y_loft_has_a_point_per_height_and_angle() {
        let ys = range(0.0, 1.0, 4);
        let g = loft_y(&ys, 8, |y, phi, _, _| DVec3::new(phi.sin(), y, phi.cos())).unwrap();
        assert_eq!((g.rows(), g.cols()), (5, 9));
        assert!((g.at(0, 0) - DVec3::new(0.0, 0.0, 1.0)).length() < 1e-12, "phi 0 is the front, +Z");
        assert!((g.at(2, 2) - DVec3::new(1.0, 0.5, 0.0)).length() < 1e-12, "a quarter turn is +X");
        assert!((g.at(4, 8) - g.at(4, 0)).length() < 1e-12, "the loft closes on itself");
    }

    #[test]
    fn a_loft_of_too_few_rows_or_segments_is_an_error_and_never_calls_the_shape() {
        let never = |_: f64, _: f64, _: usize, _: usize| -> DVec3 { panic!("shape called") };
        assert!(matches!(loft_y(&[], 8, never), Err(KitError::TooFew { .. })));
        assert!(matches!(loft_y(&[0.0], 8, never), Err(KitError::TooFew { .. })));
        assert!(matches!(loft_y(&[0.0, 1.0], 0, never), Err(KitError::TooFew { .. })));
    }

    #[test]
    fn a_segment_count_too_large_to_count_is_an_overflow_error_and_the_shape_is_never_called() {
        let never = |_: f64, _: f64, _: usize, _: usize| -> DVec3 { panic!("shape called") };
        assert!(matches!(loft_y(&[0.0, 1.0], usize::MAX, never), Err(KitError::Overflow { .. })));
        assert!(matches!(loft_y(&[0.0, 1.0], usize::MAX - 1, never), Err(KitError::Overflow { .. })));
        let profile = [(0.0, 1.0), (1.0, 1.0)];
        assert!(matches!(lathe_axis(DVec3::ZERO, DVec3::Y, &profile, usize::MAX), Err(KitError::Overflow { .. })));
    }

    #[test]
    fn a_lathe_turns_a_profile_around_any_axis() {
        let g = lathe_axis(DVec3::new(0.0, 1.0, 0.0), DVec3::X, &[(0.0, 0.5), (2.0, 0.25)], 4).unwrap();
        assert_eq!((g.rows(), g.cols()), (2, 5));
        for j in 0..5 {
            assert!((g.at(0, j).x - 0.0).abs() < 1e-12 && (g.at(1, j).x - 2.0).abs() < 1e-12, "u runs along the axis");
            let radial = g.at(0, j) - DVec3::new(0.0, 1.0, 0.0) - DVec3::X * g.at(0, j).x;
            assert!((radial.length() - 0.5).abs() < 1e-12, "the ring has the profile's radius");
        }
        assert!((g.at(0, 4) - g.at(0, 0)).length() < 1e-12);
        // Right-handed about +X: a quarter of the way round is a quarter turn from the ring's start towards its binormal.
        let (start, quarter) = (g.at(0, 0) - DVec3::Y, g.at(0, 1) - DVec3::Y);
        assert!(start.cross(quarter).dot(DVec3::X) > 0.0);
    }

    #[test]
    fn a_vertical_axis_picks_x_as_its_reference_and_a_zero_axis_is_an_error() {
        let g = lathe_axis(DVec3::ZERO, DVec3::Y, &[(0.0, 1.0), (1.0, 1.0)], 4).unwrap();
        assert!((g.at(0, 0) - DVec3::X).length() < 1e-12, "the ring starts along +X");
        assert!((g.at(0, 1) - DVec3::NEG_Z).length() < 1e-12, "and turns right-handed about +Y, towards -Z");
        let profile = [(0.0, 1.0), (1.0, 1.0)];
        assert_eq!(
            lathe_axis(DVec3::ZERO, DVec3::ZERO, &profile, 4),
            Err(KitError::Degenerate { what: "a lathe axis" })
        );
        let nan_axis = DVec3::new(f64::NAN, 0.0, 1.0);
        assert!(matches!(lathe_axis(DVec3::ZERO, nan_axis, &profile, 4), Err(KitError::Degenerate { .. })));
    }
}
