//! Caps: how the outline ends where an open contour does (a flat butt, or a round cap of two quarter-circle conics), and
//! the circle a cusp gets.
//!
//! Port of Skia (through tiny-skia). Copyright 2008 The Android Open Source Project; copyright 2020 Yevhenii Reizner.
//! BSD-3-Clause; the notice is in `raster/NOTICES.md`.

use omuzaki_core::glam::Vec2;

use super::super::path::Outline;
use super::ROOT_2_OVER_2;
use super::curves::rotate_cw;

/// Writes a cap ending at `stop` (a butt cap is a line, a round cap two quarter-circle conics).
pub(super) fn cap(outer: &mut Outline, round: bool, pivot: Vec2, normal: Vec2, stop: Vec2) {
    if round {
        let parallel = rotate_cw(normal);
        let projected_center = pivot + parallel;
        outer.conic_to(projected_center + normal, projected_center, ROOT_2_OVER_2);
        outer.conic_to(projected_center - normal, stop, ROOT_2_OVER_2);
    } else {
        outer.line_to(stop);
    }
}

/// A circle as a closed contour of four conics.
pub(super) fn push_circle(out: &mut Outline, centre: Vec2, r: f32) {
    if r.is_nan() || r <= 0.0 || !r.is_finite() || !centre.x.is_finite() || !centre.y.is_finite() {
        return;
    }
    let (left, right, top, bottom) = (centre.x - r, centre.x + r, centre.y - r, centre.y + r);
    let oval =
        [Vec2::new(centre.x, bottom), Vec2::new(left, centre.y), Vec2::new(centre.x, top), Vec2::new(right, centre.y)];
    let rect = [Vec2::new(right, bottom), Vec2::new(left, bottom), Vec2::new(left, top), Vec2::new(right, top)];
    out.move_to(oval[3]);
    for (p1, p2) in rect.iter().zip(&oval) {
        out.conic_to(*p1, *p2, ROOT_2_OVER_2);
    }
    out.close();
}
