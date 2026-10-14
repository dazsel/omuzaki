//! The curve geometry the stroker measures with: vector helpers, evaluating and classifying quads and cubics, finding
//! their inflections, cusps and points of maximum curvature, and the closeness tests that decide when an offset quad is
//! good enough. Skia's `SkStroke.cpp` and `SkGeometry.cpp`, in the same order of operations.
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

use super::{NEARLY_ZERO, PI};

/// Skia's allowance for a quad to count as a straight line (pulled out of the air there too).
const CURVATURE_SLOP: f32 = 0.000_005;

pub(super) fn is_nearly_zero(v: f32) -> bool {
    v.abs() <= NEARLY_ZERO
}

pub(super) fn cross(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

pub(super) fn rotate_cw(v: Vec2) -> Vec2 {
    Vec2::new(-v.y, v.x)
}

pub(super) fn rotate_ccw(v: Vec2) -> Vec2 {
    Vec2::new(v.y, -v.x)
}

/// A vector with a length of `length`, or `None` if it is too small, too big or not finite to scale (Skia's
/// `set_length`, which scales in doubles so a huge vector cannot overflow).
pub(super) fn with_length(v: Vec2, length: f32) -> Option<Vec2> {
    let (x, y) = (f64::from(v.x), f64::from(v.y));
    let scale = f64::from(length) / (x * x + y * y).sqrt();
    #[allow(clippy::cast_possible_truncation)] // Skia narrows the scale to f32 as well
    let scaled = Vec2::new(v.x * scale as f32, v.y * scale as f32);
    (scaled.x.is_finite() && scaled.y.is_finite() && (scaled.x != 0.0 || scaled.y != 0.0)).then_some(scaled)
}

/// Whether a vector has a direction: finite and not zero.
pub(super) fn can_normalize(v: Vec2) -> bool {
    v.x.is_finite() && v.y.is_finite() && (v.x != 0.0 || v.y != 0.0)
}

pub(super) fn degenerate_vector(v: Vec2) -> bool {
    !can_normalize(v)
}

pub(super) fn distance_sqd(a: Vec2, b: Vec2) -> f32 {
    let d = a - b;
    d.x * d.x + d.y * d.y
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Debug)]
pub(super) enum Reduction {
    /// All curve points are practically identical.
    Point,
    /// The control point is on the line between the ends.
    Line,
    /// The control point is outside the line between the ends.
    Quad,
    /// The control point is on the line but outside the ends.
    Degenerate,
    /// Two control points are on the line but outside the ends (cubic).
    Degenerate2,
    /// Three areas of max curvature found (cubic).
    Degenerate3,
}

pub(super) fn eval_quad_at(src: &[Vec2; 3], t: f32) -> Vec2 {
    let c = src[0];
    let b = (src[1] - c) * 2.0;
    let a = src[2] - src[1] * 2.0 + c;
    (a * t + b) * t + c
}

pub(super) fn eval_quad_tangent_at(src: &[Vec2; 3], t: f32) -> Vec2 {
    // The derivative is zero at t = 0 or 1 when the control point equals that end point: use the ends instead.
    if (t == 0.0 && src[0] == src[1]) || (t == 1.0 && src[1] == src[2]) {
        return src[2] - src[0];
    }
    let b = src[1] - src[0];
    let a = src[2] - src[1] - b;
    let tangent = a * t + b;
    tangent + tangent
}

pub(super) fn eval_cubic_pos_at(src: &[Vec2; 4], t: f32) -> Vec2 {
    let a = src[3] + (src[1] - src[2]) * 3.0 - src[0];
    let b = (src[2] - src[1] * 2.0 + src[0]) * 3.0;
    let c = (src[1] - src[0]) * 3.0;
    ((a * t + b) * t + c) * t + src[0]
}

pub(super) fn eval_cubic_derivative(src: &[Vec2; 4], t: f32) -> Vec2 {
    let a = src[3] + (src[1] - src[2]) * 3.0 - src[0];
    let b = (src[2] - src[1] * 2.0 + src[0]) * 2.0;
    let c = src[1] - src[0];
    (a * t + b) * t + c
}

pub(super) fn eval_cubic_tangent_at(src: &[Vec2; 4], t: f32) -> Vec2 {
    // The derivative is zero at t = 0 or 1 when the adjacent control point equals the end point: use the next control
    // point or the ends to compute the tangent.
    if (t == 0.0 && src[0] == src[1]) || (t == 1.0 && src[2] == src[3]) {
        let mut tangent = if t == 0.0 { src[2] - src[0] } else { src[3] - src[1] };
        if tangent.x == 0.0 && tangent.y == 0.0 {
            tangent = src[3] - src[0];
        }
        tangent
    } else {
        eval_cubic_derivative(src, t)
    }
}

/// Where a quad's curvature is greatest: `t = -(A . B) / |B|^2` for `A = p1 - p0`, `B = p0 - 2 p1 + p2`.
pub(super) fn find_quad_max_curvature(src: &[Vec2; 3]) -> f32 {
    let a = src[1] - src[0];
    let b = src[0] - src[1] - src[1] + src[2];
    let mut numer = -(a.x * b.x + a.y * b.y);
    let mut denom = b.x * b.x + b.y * b.y;
    if denom < 0.0 {
        numer = -numer;
        denom = -denom;
    }
    if numer <= 0.0 {
        return 0.0;
    }
    if numer >= denom {
        // Also catches denom = 0.
        return 1.0;
    }
    numer / denom
}

/// Roots of F' . F'' for a cubic, the points of maximum curvature, inside (0, 1) after clamping; returns how many.
pub(in crate::raster) fn find_cubic_max_curvature(src: &[Vec2; 4], roots: &mut [f32; 3]) -> usize {
    let formulate = |s: [f32; 4]| {
        let a = s[1] - s[0];
        let b = s[2] - 2.0 * s[1] + s[0];
        let c = s[3] + 3.0 * (s[1] - s[2]) - s[0];
        [c * c, 3.0 * b * c, 2.0 * b * b + c * a, a * b]
    };
    let cx = formulate([src[0].x, src[1].x, src[2].x, src[3].x]);
    let cy = formulate([src[0].y, src[1].y, src[2].y, src[3].y]);
    let coeff = [cx[0] + cy[0], cx[1] + cy[1], cx[2] + cy[2], cx[3] + cy[3]];
    solve_cubic_poly(&coeff, roots)
}

/// Solves `c0 t^3 + c1 t^2 + c2 t + c3 = 0`, returning the distinct roots, clamped to 0..=1 and sorted.
pub(super) fn solve_cubic_poly(coeff: &[f32; 4], roots: &mut [f32; 3]) -> usize {
    if is_nearly_zero(coeff[0]) {
        // Just a quadratic.
        return crate::raster::geom::find_unit_quad_roots(coeff[1], coeff[2], coeff[3], roots);
    }
    let inva = coeff[0].recip();
    let a = coeff[1] * inva;
    let b = coeff[2] * inva;
    let c = coeff[3] * inva;
    let q = (a * a - b * 3.0) / 9.0;
    let r = (2.0 * a * a * a - 9.0 * a * b + 27.0 * c) / 54.0;
    let q3 = q * q * q;
    let r2_minus_q3 = r * r - q3;
    let adiv3 = a / 3.0;

    if r2_minus_q3 < 0.0 {
        // Three real roots. The divide and root can, to finite precision, be slightly outside -1..1.
        let theta = (r / q3.sqrt()).clamp(-1.0, 1.0);
        let theta = libm::acosf(theta);
        let neg2_root_q = -2.0 * q.sqrt();
        roots[0] = (neg2_root_q * libm::cosf(theta / 3.0) - adiv3).clamp(0.0, 1.0);
        roots[1] = (neg2_root_q * libm::cosf((theta + 2.0 * PI) / 3.0) - adiv3).clamp(0.0, 1.0);
        roots[2] = (neg2_root_q * libm::cosf((theta - 2.0 * PI) / 3.0) - adiv3).clamp(0.0, 1.0);
        // Sort, then collapse duplicates.
        if roots[0] > roots[1] {
            roots.swap(0, 1);
        }
        if roots[1] > roots[2] {
            roots.swap(1, 2);
        }
        if roots[0] > roots[1] {
            roots.swap(0, 1);
        }
        let mut len = 3;
        if roots[1] == roots[2] {
            len = 2;
        }
        if roots[0] == roots[1] {
            len = 1;
        }
        len
    } else {
        // One real root.
        let mut a2 = r.abs() + r2_minus_q3.sqrt();
        a2 = libm::powf(a2, 0.333_333_3);
        if r > 0.0 {
            a2 = -a2;
        }
        if a2 != 0.0 {
            a2 += q / a2;
        }
        roots[0] = (a2 - adiv3).clamp(0.0, 1.0);
        1
    }
}

/// Where a cubic's curvature is zero: the roots inside (0, 1) of `(Bx Cy - By Cx) t^2 + (Ax Cy - Ay Cx) t + Ax By - Ay Bx`.
pub(super) fn find_cubic_inflections(src: &[Vec2; 4], roots: &mut [f32; 3]) -> usize {
    let a = src[1] - src[0];
    let b = src[2] - 2.0 * src[1] + src[0];
    let c = src[3] + 3.0 * (src[1] - src[2]) - src[0];
    crate::raster::geom::find_unit_quad_roots(
        b.x * c.y - b.y * c.x,
        a.x * c.y - a.y * c.x,
        a.x * b.y - a.y * b.x,
        roots,
    )
}

/// Where a cubic has a cusp, if it does.
pub(super) fn find_cubic_cusp(src: &[Vec2; 4]) -> Option<f32> {
    // An adjacent control point equal to its end behaves like a cusp at t = 0 or 1; such cubics are skipped.
    if src[0] == src[1] || src[2] == src[3] {
        return None;
    }
    // Only cubics whose control lines cross can have one.
    if on_same_side(src, 0, 2) || on_same_side(src, 2, 0) {
        return None;
    }
    let mut roots = [0.0_f32; 3];
    let n = find_cubic_max_curvature(src, &mut roots);
    for &t in &roots[..n] {
        if t <= 0.0 || t >= 1.0 {
            continue;
        }
        let d = eval_cubic_derivative(src, t);
        let precision =
            (distance_sqd(src[1], src[0]) + distance_sqd(src[2], src[1]) + distance_sqd(src[3], src[2])) * 1e-8;
        if d.length_squared() < precision {
            return Some(t);
        }
    }
    None
}

/// Whether both points `src[test]`, `src[test + 1]` are in the same half plane of the segment `src[line]`, `src[line + 1]`.
pub(super) fn on_same_side(src: &[Vec2; 4], test_index: usize, line_index: usize) -> bool {
    let origin = src[line_index];
    let line = src[line_index + 1] - origin;
    let crosses = [0, 1].map(|i| cross(line, src[test_index + i] - origin));
    crosses[0] * crosses[1] >= 0.0
}

pub(super) fn check_quad_linear(quad: &[Vec2; 3]) -> (Vec2, Reduction) {
    let degenerate_ab = degenerate_vector(quad[1] - quad[0]);
    let degenerate_bc = degenerate_vector(quad[2] - quad[1]);
    if degenerate_ab & degenerate_bc {
        return (Vec2::ZERO, Reduction::Point);
    }
    if degenerate_ab | degenerate_bc {
        return (Vec2::ZERO, Reduction::Line);
    }
    if !quad_in_line(quad) {
        return (Vec2::ZERO, Reduction::Quad);
    }
    let t = find_quad_max_curvature(quad);
    if t == 0.0 || t == 1.0 {
        return (Vec2::ZERO, Reduction::Line);
    }
    (eval_quad_at(quad, t), Reduction::Degenerate)
}

/// Whether all three points of a quad are in a line: the inside point is close to the line between the outermost.
pub(super) fn quad_in_line(quad: &[Vec2; 3]) -> bool {
    let mut pt_max = -1.0;
    let (mut outer1, mut outer2) = (0, 0);
    for index in 0..2 {
        for inner in index + 1..3 {
            let diff = quad[inner] - quad[index];
            let test_max = diff.x.abs().max(diff.y.abs());
            if pt_max < test_max {
                outer1 = index;
                outer2 = inner;
                pt_max = test_max;
            }
        }
    }
    let mid = outer1 ^ outer2 ^ 3;
    let line_slop = pt_max * pt_max * CURVATURE_SLOP;
    pt_to_line(quad[mid], quad[outer1], quad[outer2]) <= line_slop
}

/// The squared distance from the point to the segment.
pub(super) fn pt_to_line(pt: Vec2, line_start: Vec2, line_end: Vec2) -> f32 {
    let dxy = line_end - line_start;
    let ab0 = pt - line_start;
    let numer = dxy.dot(ab0);
    let denom = dxy.dot(dxy);
    let t = numer / denom;
    if (0.0..=1.0).contains(&t) {
        let hit = Vec2::new(line_start.x * (1.0 - t) + line_end.x * t, line_start.y * (1.0 - t) + line_end.y * t);
        distance_sqd(hit, pt)
    } else {
        distance_sqd(pt, line_start)
    }
}

/// Intersects the line with the quad; the t values on the quad where the line crosses.
pub(super) fn intersect_quad_ray(line: &[Vec2; 2], quad: &[Vec2; 3], roots: &mut [f32; 3]) -> usize {
    let vec = line[1] - line[0];
    let r = [0, 1, 2].map(|n| (quad[n].y - line[0].y) * vec.x - (quad[n].x - line[0].x) * vec.y);
    let mut a = r[2];
    let mut b = r[1];
    let c = r[0];
    a += c - 2.0 * b; // A = a - 2b + c
    b -= c; // B = -(b - c)
    crate::raster::geom::find_unit_quad_roots(a, 2.0 * b, c, roots)
}

pub(super) fn points_within_dist(near_pt: Vec2, far_pt: Vec2, limit: f32) -> bool {
    distance_sqd(near_pt, far_pt) <= limit * limit
}

pub(super) fn sharp_angle(quad: &[Vec2; 3]) -> bool {
    let mut smaller = quad[1] - quad[0];
    let mut larger = quad[1] - quad[2];
    let smaller_len = smaller.length_squared();
    let mut larger_len = larger.length_squared();
    if smaller_len > larger_len {
        std::mem::swap(&mut smaller, &mut larger);
        larger_len = smaller_len;
    }
    let Some(scaled) = with_length(smaller, larger_len) else { return false };
    scaled.dot(larger) > 0.0
}

/// Whether the point is close to the bounds of the quad (a quick reject).
pub(super) fn pt_in_quad_bounds(quad: &[Vec2; 3], pt: Vec2, inv_res_scale: f32) -> bool {
    let x_min = quad[0].x.min(quad[1].x).min(quad[2].x);
    if pt.x + inv_res_scale < x_min {
        return false;
    }
    let x_max = quad[0].x.max(quad[1].x).max(quad[2].x);
    if pt.x - inv_res_scale > x_max {
        return false;
    }
    let y_min = quad[0].y.min(quad[1].y).min(quad[2].y);
    if pt.y + inv_res_scale < y_min {
        return false;
    }
    let y_max = quad[0].y.max(quad[1].y).max(quad[2].y);
    pt.y - inv_res_scale <= y_max
}

pub(super) fn check_cubic_linear(cubic: &[Vec2; 4], reduction: &mut [Vec2; 3]) -> (Reduction, Vec2) {
    let degenerate_ab = degenerate_vector(cubic[1] - cubic[0]);
    let degenerate_bc = degenerate_vector(cubic[2] - cubic[1]);
    let degenerate_cd = degenerate_vector(cubic[3] - cubic[2]);
    if degenerate_ab & degenerate_bc & degenerate_cd {
        return (Reduction::Point, Vec2::ZERO);
    }
    if i32::from(degenerate_ab) + i32::from(degenerate_bc) + i32::from(degenerate_cd) == 2 {
        return (Reduction::Line, Vec2::ZERO);
    }
    if !cubic_in_line(cubic) {
        return (Reduction::Quad, if degenerate_ab { cubic[2] } else { cubic[1] });
    }
    let mut roots = [0.0_f32; 3];
    let n = find_cubic_max_curvature(cubic, &mut roots);
    let mut r_count = 0;
    // Reject any t that evaluates to either end point.
    for &t in &roots[..n] {
        if t <= 0.0 || t >= 1.0 {
            continue;
        }
        reduction[r_count] = eval_cubic_pos_at(cubic, t);
        if reduction[r_count] != cubic[0] && reduction[r_count] != cubic[3] {
            r_count += 1;
        }
    }
    let kind = match r_count {
        0 => Reduction::Line,
        1 => Reduction::Degenerate,
        2 => Reduction::Degenerate2,
        _ => Reduction::Degenerate3,
    };
    (kind, Vec2::ZERO)
}

/// Whether all four points of a cubic are in a line: both inner points are close to the line between the outermost.
pub(super) fn cubic_in_line(cubic: &[Vec2; 4]) -> bool {
    let mut pt_max = -1.0;
    let (mut outer1, mut outer2) = (0, 0);
    for index in 0..3 {
        for inner in index + 1..4 {
            let diff = cubic[inner] - cubic[index];
            let test_max = diff.x.abs().max(diff.y.abs());
            if pt_max < test_max {
                outer1 = index;
                outer2 = inner;
                pt_max = test_max;
            }
        }
    }
    let mid1 = (1 + (2 >> outer2)) >> outer1;
    let mid2 = outer1 ^ outer2 ^ mid1;
    let line_slop = pt_max * pt_max * 0.00001;
    pt_to_line(cubic[mid1], cubic[outer1], cubic[outer2]) <= line_slop
        && pt_to_line(cubic[mid2], cubic[outer1], cubic[outer2]) <= line_slop
}
