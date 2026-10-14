//! Curve geometry the scan converter and the stroker share: chopping Béziers, splitting conics into quads.
//!
//! The functions follow Skia's `SkGeometry` (and tiny-skia's port of it, `tiny-skia-path/src/path_geometry.rs`) operation
//! for operation, in `f32`, because the scan converter reproduces Skia's pixels and a different chop point moves an edge.
//!
//! Port of Skia's `SkGeometry` (through tiny-skia's `path_geometry.rs`). Copyright 2006 The Android Open Source Project; copyright 2020 Yevhenii
//! Reizner. BSD-3-Clause; the notice is in `raster/NOTICES.md`.

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

/// `numer / denom` if it lies strictly between 0 and 1 (Skia's `valid_unit_divide`).
pub(super) fn valid_unit_divide(mut numer: f32, mut denom: f32) -> Option<f32> {
    if numer < 0.0 {
        numer = -numer;
        denom = -denom;
    }
    if denom == 0.0 || numer == 0.0 || numer >= denom {
        return None;
    }
    let r = numer / denom;
    (r > 0.0 && r < 1.0).then_some(r)
}

/// Roots of `a t^2 + b t + c` that lie strictly inside (0, 1), sorted, without a double root. Returns how many.
pub(super) fn find_unit_quad_roots(a: f32, b: f32, c: f32, roots: &mut [f32; 3]) -> usize {
    if a == 0.0 {
        return match valid_unit_divide(-c, b) {
            Some(r) => {
                roots[0] = r;
                1
            }
            None => 0,
        };
    }
    // Doubles, so the discriminant cannot overflow.
    let dr = f64::from(b) * f64::from(b) - 4.0 * f64::from(a) * f64::from(c);
    if dr < 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation)] // Skia narrows the root to f32 as well
    let r = dr.sqrt() as f32;
    if !r.is_finite() {
        return 0;
    }
    let q = if b < 0.0 { -(b - r) / 2.0 } else { -(b + r) / 2.0 };
    let mut n = 0;
    if let Some(root) = valid_unit_divide(q, a) {
        roots[n] = root;
        n += 1;
    }
    if let Some(root) = valid_unit_divide(c, q) {
        roots[n] = root;
        n += 1;
    }
    if n == 2 {
        if roots[0] > roots[1] {
            roots.swap(0, 1);
        } else if roots[0] == roots[1] {
            n -= 1;
        }
    }
    n
}

fn interp(a: Vec2, b: Vec2, t: f32) -> Vec2 {
    a + (b - a) * t
}

/// Splits a quad at `t`: the five points of the two halves, sharing the middle one.
pub(super) fn chop_quad_at(src: [Vec2; 3], t: f32) -> [Vec2; 5] {
    let p01 = interp(src[0], src[1], t);
    let p12 = interp(src[1], src[2], t);
    [src[0], p01, interp(p01, p12, t), p12, src[2]]
}

/// Splits a cubic at `t`: the seven points of the two halves, sharing the middle one.
fn chop_cubic_at_one(src: [Vec2; 4], t: f32) -> [Vec2; 7] {
    let ab = interp(src[0], src[1], t);
    let bc = interp(src[1], src[2], t);
    let cd = interp(src[2], src[3], t);
    let abc = interp(ab, bc, t);
    let bcd = interp(bc, cd, t);
    [src[0], ab, abc, interp(abc, bcd, t), bcd, cd, src[3]]
}

fn is_not_monotonic(a: f32, b: f32, c: f32) -> bool {
    let ab = a - b;
    let mut bc = b - c;
    if ab < 0.0 {
        bc = -bc;
    }
    ab == 0.0 || bc < 0.0
}

/// Chops a quad so each piece is monotonic in y, with the extremum's tangent made exactly flat. Returns the points
/// of one or two quads (`[p0 p1 p2]` or `[p0 p1 p2 p3 p4]`) and how many quads.
pub(super) fn chop_quad_at_y_extrema(src: [Vec2; 3]) -> ([Vec2; 5], usize) {
    let (a, mut b, c) = (src[0].y, src[1].y, src[2].y);
    if is_not_monotonic(a, b, c) {
        if let Some(t) = valid_unit_divide(a - b, a - b - b + c) {
            let mut dst = chop_quad_at(src, t);
            dst[1].y = dst[2].y;
            dst[3].y = dst[2].y;
            return (dst, 2);
        }
        // Underflow: force the quad monotonic by moving the control point to the nearer end.
        b = if (a - b).abs() < (b - c).abs() { a } else { c };
    }
    ([Vec2::new(src[0].x, a), Vec2::new(src[1].x, b), Vec2::new(src[2].x, c), Vec2::ZERO, Vec2::ZERO], 1)
}

/// Chops a cubic at up to two parameters (sorted, inside (0, 1)). Returns the points of the cubics, laid out
/// `[p0 p1 p2 p3 | p4 p5 p6 | p7 p8 p9]`; `ts.len() + 1` cubics.
pub(super) fn chop_cubic_at(src: [Vec2; 4], ts: &[f32]) -> [Vec2; 10] {
    let mut dst = [Vec2::ZERO; 10];
    match ts {
        [] => dst[..4].copy_from_slice(&src),
        [t] => dst[..7].copy_from_slice(&chop_cubic_at_one(src, *t)),
        [t0, t1] => {
            let first = chop_cubic_at_one(src, *t0);
            dst[..4].copy_from_slice(&first[..4]);
            let rest = [first[3], first[4], first[5], first[6]];
            if let Some(t) = valid_unit_divide(t1 - t0, 1.0 - t0) {
                dst[3..10].copy_from_slice(&chop_cubic_at_one(rest, t));
            } else {
                // The renormalised parameter is out of range (finite precision): a degenerate last cubic.
                dst[3..7].copy_from_slice(&rest);
                dst[7..10].fill(rest[3]);
            }
        }
        _ => unreachable!("a cubic is chopped at at most two parameters"),
    }
    dst
}

/// Chops a cubic so each piece is monotonic in y, with the extremum's tangent made exactly flat. Returns the points of up
/// to three cubics, laid out `[p0 p1 p2 p3 | p4 p5 p6 | p7 p8 p9]`, and how many cubics.
pub(super) fn chop_cubic_at_y_extrema(src: [Vec2; 4]) -> ([Vec2; 10], usize) {
    let (a, b, c, d) = (src[0].y, src[1].y, src[2].y, src[3].y);
    // Cubic'(t) = A t^2 + B t + C with A = 3(-a + 3(b - c) + d), B = 6(a - 2b + c), C = 3(b - a); Skia drops the 3.
    let (na, nb, nc) = (d - a + 3.0 * (b - c), 2.0 * (a - b - b + c), b - a);
    let mut roots = [0.0_f32; 3];
    let n = find_unit_quad_roots(na, nb, nc, &mut roots);
    let mut dst = chop_cubic_at(src, &roots[..n]);
    if n >= 1 {
        dst[2].y = dst[3].y;
        dst[4].y = dst[3].y;
        if n == 2 {
            dst[5].y = dst[6].y;
            dst[7].y = dst[6].y;
        }
    }
    (dst, n + 1)
}

/// Whether `b` lies between `a` and `c` (inclusive, either order).
fn between(a: f32, b: f32, c: f32) -> bool {
    (a - b) * (c - b) <= 0.0
}

/// A rational quadratic Bézier: the exact arc a stroker's round caps and joins are made of.
#[derive(Copy, Clone, Debug)]
pub(super) struct Conic {
    pub points: [Vec2; 3],
    pub weight: f32,
}

impl Conic {
    fn chop(&self) -> (Conic, Conic) {
        let scale = 1.0 / (1.0 + self.weight);
        let new_w = (0.5 + self.weight * 0.5).sqrt();
        let [p0, p1, p2] = self.points;
        let wp1 = p1 * self.weight;
        let m = (p0 + wp1 * 2.0 + p2) * scale * 0.5;
        (
            Conic { points: [p0, (p0 + wp1) * scale, m], weight: new_w },
            Conic { points: [m, (wp1 + p2) * scale, p2], weight: new_w },
        )
    }

    /// The number of halvings Skia uses to approximate this conic by quads within `tolerance` (Floater's bound).
    fn quad_pow2(&self, tolerance: f32) -> usize {
        const MAX_POW2: usize = 4;
        let a = self.weight - 1.0;
        let k = a / (4.0 * (2.0 + a));
        let [p0, p1, p2] = self.points;
        let d = (p0 - p1 * 2.0 + p2) * k;
        let mut error = d.length();
        let mut pow2 = 0;
        while pow2 < MAX_POW2 && error > tolerance {
            error *= 0.25;
            pow2 += 1;
        }
        pow2
    }

    /// Splits into `2^level` quads, writing each quad's control and end point (the start is the previous end).
    fn subdivide(&self, out: &mut [Vec2], level: usize) -> usize {
        if level == 0 {
            out[0] = self.points[1];
            out[1] = self.points[2];
            return 2;
        }
        let (mut first, mut second) = self.chop();
        let (start_y, end_y) = (self.points[0].y, self.points[2].y);
        if between(start_y, self.points[1].y, end_y) {
            // A monotonic conic must stay monotonic when chopped, or the scan converter would walk it backwards.
            let mid_y = first.points[2].y;
            if !between(start_y, mid_y, end_y) {
                let closer = if (mid_y - start_y).abs() < (mid_y - end_y).abs() { start_y } else { end_y };
                first.points[2].y = closer;
                second.points[0].y = closer;
            }
            if !between(start_y, first.points[1].y, first.points[2].y) {
                first.points[1].y = start_y;
            }
            if !between(second.points[0].y, second.points[1].y, end_y) {
                second.points[1].y = end_y;
            }
        }
        let n = first.subdivide(out, level - 1);
        n + second.subdivide(&mut out[n..], level - 1)
    }

    /// Like [`Conic::quads`] but with at least `min_pow2` halvings: tiny-skia's stroker converts conics with at least one,
    /// which tests need to compare outlines.
    #[cfg(test)]
    pub fn quads_at_least(self, min_pow2: usize, out: &mut [Vec2; 33]) -> usize {
        let pow2 = self.quad_pow2(0.25).max(min_pow2);
        out[0] = self.points[0];
        self.subdivide(&mut out[1..], pow2);
        1 << pow2
    }

    /// The quads Skia's edge builder replaces this conic with (tolerance a quarter pixel): `1 + 2 n` points for `n`
    /// quads, written to `out` (at most 33). Returns the number of quads.
    pub fn quads(self, out: &mut [Vec2; 33]) -> usize {
        let pow2 = self.quad_pow2(0.25);
        out[0] = self.points[0];
        let written = self.subdivide(&mut out[1..], pow2);
        debug_assert_eq!(written, 2 << pow2);
        1 << pow2
    }
}
