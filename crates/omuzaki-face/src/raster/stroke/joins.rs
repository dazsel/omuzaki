//! Joins: what the outline does at a corner of the path (a miter up to the limit, a round arc, a blunt bevel), and the
//! helpers they share (the unit arc a round join or cap is cut from, normals).
//!
//! Port of Skia (through tiny-skia). Copyright 2008 The Android Open Source Project; copyright 2020 Yevhenii Reizner.
//! BSD-3-Clause; the notice is in `raster/NOTICES.md`.

// The numerics follow Skia operation for operation: its short names (`a`, `b`, `x0`, `dx`), its exact float comparisons
// (a vertical edge is exactly vertical, a weight is exactly 1) and its midpoint arithmetic are part of what is reproduced.
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::float_cmp,
    clippy::manual_midpoint,
    clippy::neg_cmp_op_on_partial_ord
)]

use omuzaki_core::glam::Vec2;

use super::super::path::Outline;
use super::curves::{can_normalize, cross, is_nearly_zero, rotate_ccw, with_length};
use super::{NEARLY_ZERO, QUADRANT_POINTS, ROOT_2_OVER_2};

/// The two builders a joiner writes to; the join swaps them when the path turns the other way.
pub(super) struct Builders<'a> {
    pub(super) inner: &'a mut Outline,
    pub(super) outer: &'a mut Outline,
}

impl Builders<'_> {
    #[allow(clippy::mut_mut)] // swapping which builder is which, not what they hold
    fn swap(&mut self) {
        std::mem::swap(&mut self.inner, &mut self.outer);
    }
}

pub(super) fn is_clockwise(before: Vec2, after: Vec2) -> bool {
    before.x * after.y > before.y * after.x
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub(super) enum AngleType {
    Nearly180,
    Sharp,
    Shallow,
    NearlyLine,
}

pub(super) fn dot_to_angle_type(dot: f32) -> AngleType {
    if dot >= 0.0 {
        if is_nearly_zero(1.0 - dot) { AngleType::NearlyLine } else { AngleType::Shallow }
    } else if is_nearly_zero(1.0 + dot) {
        AngleType::Nearly180
    } else {
        AngleType::Sharp
    }
}

/// In the degenerate case that the stroke radius is larger than the segments, joining the two inner segments directly
/// may show through as a diagonal; going through the pivot avoids that at the cost of an extra edge.
pub(super) fn handle_inner_join(pivot: Vec2, after: Vec2, inner: &mut Outline) {
    inner.line_to(pivot);
    inner.line_to(pivot - after);
}

pub(super) fn round_joiner(
    before_unit_normal: Vec2,
    pivot: Vec2,
    after_unit_normal: Vec2,
    radius: f32,
    builders: &mut Builders<'_>,
) {
    let dot_prod = before_unit_normal.dot(after_unit_normal);
    if dot_to_angle_type(dot_prod) == AngleType::NearlyLine {
        return;
    }
    let (mut before, mut after) = (before_unit_normal, after_unit_normal);
    let mut ccw = false;
    if !is_clockwise(before, after) {
        builders.swap();
        before = -before;
        after = -after;
        ccw = true;
    }

    let mut conics = [crate::raster::geom::Conic { points: [Vec2::ZERO; 3], weight: 0.0 }; 5];
    let n = build_unit_arc(before, after, ccw, radius, pivot, &mut conics);
    if n > 0 {
        for conic in &conics[..n] {
            builders.outer.conic_to(conic.points[1], conic.points[2], conic.weight);
        }
        handle_inner_join(pivot, after * radius, builders.inner);
    }
}

/// The conics (one per quadrant, plus one for what is left) of the unit arc from `u_start` to `u_stop`, scaled by `radius`
/// about `pivot`. Returns how many; none if the vectors are coincident.
pub(super) fn build_unit_arc(
    u_start: Vec2,
    u_stop: Vec2,
    ccw: bool,
    radius: f32,
    pivot: Vec2,
    dst: &mut [crate::raster::geom::Conic; 5],
) -> usize {
    use crate::raster::geom::Conic;
    // Rotate by (x, y) so that u_start is (1, 0).
    let x = u_start.dot(u_stop);
    let mut y = cross(u_start, u_stop);
    let abs_y = y.abs();

    // Effectively coincident vectors: the angle is nearly 0 (x > 0), or nearly 180 (y == 0).
    if abs_y <= NEARLY_ZERO && x > 0.0 && ((y >= 0.0 && !ccw) || (y <= 0.0 && ccw)) {
        return 0;
    }
    if ccw {
        y = -y;
    }

    // One conic per quadrant of a circle. Which quadrant does (x, y) lie in?
    let mut quadrant = 0;
    if y == 0.0 {
        quadrant = 2; // 180
    } else if x == 0.0 {
        quadrant = if y > 0.0 { 1 } else { 3 }; // 90 or 270
    } else {
        if y < 0.0 {
            quadrant += 2;
        }
        if (x < 0.0) != (y < 0.0) {
            quadrant += 1;
        }
    }

    let mut conic_count = quadrant;
    for i in 0..conic_count {
        dst[i] = Conic {
            points: [QUADRANT_POINTS[i * 2], QUADRANT_POINTS[i * 2 + 1], QUADRANT_POINTS[(i * 2 + 2) % 8]],
            weight: ROOT_2_OVER_2,
        };
    }

    // Any remaining (sub-90-degree) arc for the last conic.
    let final_pt = Vec2::new(x, y);
    let last_q = QUADRANT_POINTS[(quadrant * 2) % 8]; // already a unit vector
    let dot = last_q.dot(final_pt);
    if dot < 1.0 {
        let mut off_curve = Vec2::new(last_q.x + x, last_q.y + y);
        // The bisector, rescaled to be the off-curve point: its length is 1 / cos(theta / 2), and so is the weight.
        let cos_theta_over_2 = ((1.0 + dot) / 2.0).sqrt();
        off_curve = with_length(off_curve, cos_theta_over_2.recip()).unwrap_or(Vec2::ZERO);
        if can_normalize(last_q - off_curve) {
            dst[conic_count] = Conic { points: [last_q, off_curve, final_pt], weight: cos_theta_over_2 };
            conic_count += 1;
        }
    }

    // Now the counter-clockwise flip, the rotation to u_start and the user's scale and translation.
    let (sin, cos) = (u_start.y, u_start.x);
    let map = |p: Vec2| {
        let p = if ccw { Vec2::new(p.x, -p.y) } else { p };
        let rotated = Vec2::new(cos * p.x - sin * p.y, sin * p.x + cos * p.y);
        rotated * radius + pivot
    };
    for conic in &mut dst[..conic_count] {
        conic.points = conic.points.map(map);
    }
    conic_count
}

#[allow(clippy::too_many_arguments)] // Skia's joiner signature
pub(super) fn miter_joiner(
    before_unit_normal: Vec2,
    pivot: Vec2,
    after_unit_normal: Vec2,
    radius: f32,
    inv_miter_limit: f32,
    prev_is_line: bool,
    mut curr_is_line: bool,
    builders: &mut Builders<'_>,
) {
    // The dot is of normals, not tangents, so its sign is opposite to the tangents'.
    let dot_prod = before_unit_normal.dot(after_unit_normal);
    let angle_type = dot_to_angle_type(dot_prod);
    let (mut before, mut after) = (before_unit_normal, after_unit_normal);

    if angle_type == AngleType::NearlyLine {
        return;
    }

    if angle_type == AngleType::Nearly180 {
        curr_is_line = false;
        blunt(builders, pivot, radius, curr_is_line, after);
        return;
    }

    let ccw = !is_clockwise(before, after);
    if ccw {
        builders.swap();
        before = -before;
        after = -after;
    }

    // An upright right angle (the common case of stroking rectangles) is special-cased for speed and accuracy.
    if dot_prod == 0.0 && inv_miter_limit <= ROOT_2_OVER_2 {
        miter(builders, pivot, radius, prev_is_line, curr_is_line, (before + after) * radius, after);
        return;
    }

    // The most accurate way to form the initial mid-vector.
    let mut mid = if angle_type == AngleType::Sharp {
        let m = Vec2::new(after.y - before.y, before.x - after.x);
        if ccw { -m } else { m }
    } else {
        Vec2::new(before.x + after.x, before.y + after.y)
    };

    // midLength = radius / sinHalfAngle; abort (bevel) if that exceeds the miter limit times the radius.
    let sin_half_angle = ((1.0 + dot_prod) * 0.5).sqrt();
    if sin_half_angle < inv_miter_limit {
        curr_is_line = false;
        blunt(builders, pivot, radius, curr_is_line, after);
        return;
    }

    if let Some(scaled) = with_length(mid, radius / sin_half_angle) {
        mid = scaled;
    } else {
        mid = Vec2::ZERO;
    }
    miter(builders, pivot, radius, prev_is_line, curr_is_line, mid, after);
}

/// A corner past the miter limit (or a reversal): the outer edge goes straight to the new side.
pub(super) fn blunt(builders: &mut Builders<'_>, pivot: Vec2, radius: f32, curr_is_line: bool, after: Vec2) {
    let after = after * radius;
    if !curr_is_line {
        builders.outer.line_to(pivot + after);
    }
    handle_inner_join(pivot, after, builders.inner);
}

pub(super) fn miter(
    builders: &mut Builders<'_>,
    pivot: Vec2,
    radius: f32,
    prev_is_line: bool,
    curr_is_line: bool,
    mid: Vec2,
    after: Vec2,
) {
    let after = after * radius;
    if prev_is_line {
        builders.outer.set_last_point(pivot + mid);
    } else {
        builders.outer.line_to(pivot + mid);
    }
    if !curr_is_line {
        builders.outer.line_to(pivot + after);
    }
    handle_inner_join(pivot, after, builders.inner);
}

/// The normal and unit normal of the segment `before` to `after`, scaled by `radius`; `None` if it has no direction.
pub(super) fn set_normal_unit_normal(before: Vec2, after: Vec2, scale: f32, radius: f32) -> Option<(Vec2, Vec2)> {
    let unit = with_length((after - before) * scale, 1.0)?;
    let unit_normal = rotate_ccw(unit);
    Some((unit_normal * radius, unit_normal))
}

pub(super) fn set_normal_unit_normal2(vec: Vec2, radius: f32) -> Option<(Vec2, Vec2)> {
    let unit = with_length(vec, 1.0)?;
    let unit_normal = rotate_ccw(unit);
    Some((unit_normal * radius, unit_normal))
}
