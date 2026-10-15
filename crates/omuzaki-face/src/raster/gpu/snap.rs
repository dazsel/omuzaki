//! Skia's edge snapping for the GPU encoder: a path in pixel space with every curve replaced by the chain of line
//! segments Skia would build for it, and the y of every vertex snapped to the nearest quarter pixel.
//!
//! Vello's coverage is exact, Skia's is not (`raster::scan` explains the steps). To land on the same pixels as the CPU
//! raster the encoder hands Vello what Skia hands its scan converter: lines, `y` at quarter rows, and for a curve the
//! segment count of Skia's `diff_to_shift` with vertices at equal steps of `t`. Skia reaches its vertices by fixed-point
//! forward differencing; evaluating the curve at the same `t` steps lands within a thousandth of a pixel of them.
//! Not reproduced: the rule that rounds a long, steep quad step to a whole row (`quad_chain`'s), which only matters for
//! quads taller than two rows per segment; the face painter builds cubics.

// The fixed-point setup follows Skia's own short names (`a`..`d` are the four control coordinates), as `raster::scan` does.
#![allow(clippy::many_single_char_names)]

use omuzaki_face_api::PathEl;
use vello::kurbo::{Affine, BezPath, Point};

use super::gradient::point;

/// The most halvings Skia's edge setup allows a curve (`MAX_COEFF_SHIFT`).
const MAX_SHIFT: u32 = 6;

/// `y` rounded to the nearest quarter, halves up, as Skia's `SnapY` does for anti-aliased edges.
pub(super) fn snap_quarter(y: f64) -> f64 {
    (y * 4.0 + 0.5).floor() / 4.0
}

/// Refills `out` with `els` in pixel space under `to_px`, snapped as above. Control points never reach `out`.
pub(super) fn snapped_path(els: &[PathEl], to_px: Affine, out: &mut BezPath) {
    let map = |p| to_px * point(p);
    let snapped = |p: Point| Point::new(p.x, snap_quarter(p.y));
    out.truncate(0);
    let mut current = Point::ZERO;
    for el in els {
        match *el {
            PathEl::Move(a) => {
                current = map(a);
                out.move_to(snapped(current));
            }
            PathEl::Line(a) => {
                current = map(a);
                out.line_to(snapped(current));
            }
            PathEl::Quad(a, b) => {
                let end = map(b);
                chain(&[current, map(a), end], quad_shift, out);
                current = end;
            }
            PathEl::Cubic(a, b, c) => {
                let end = map(c);
                chain(&[current, map(a), map(b), end], cubic_shift, out);
                current = end;
            }
            PathEl::Close => out.close_path(),
        }
    }
}

/// Appends the chain for one Bézier (2 control points: quad, 3: cubic), cut where it turns in y so each piece is monotonic.
fn chain(p: &[Point], shift: fn(&[Point]) -> u32, out: &mut BezPath) {
    let mut pieces = [[Point::ZERO; 4]; 3];
    let n = chop_at_y_extrema(p, &mut pieces);
    for piece in &pieces[..n] {
        let piece = &piece[..p.len()];
        let steps = 1_u32 << shift(piece);
        for i in 1..=steps {
            let at = bezier(piece, f64::from(i) / f64::from(steps));
            out.line_to(Point::new(at.x, snap_quarter(at.y)));
        }
    }
}

/// The point at `t` (de Casteljau).
fn bezier(p: &[Point], t: f64) -> Point {
    let mut tmp = [Point::ZERO; 4];
    tmp[..p.len()].copy_from_slice(p);
    for level in (1..p.len()).rev() {
        for i in 0..level {
            tmp[i] = tmp[i].lerp(tmp[i + 1], t);
        }
    }
    tmp[0]
}

/// Splits `p` at its y extrema into at most three monotonic pieces (written to `out`); returns how many.
fn chop_at_y_extrema(p: &[Point], out: &mut [[Point; 4]; 3]) -> usize {
    let (mut roots, mut count) = ([0.0_f64; 2], 0);
    let mut ys = [0.0; 4];
    for (slot, q) in ys.iter_mut().zip(p) {
        *slot = q.y;
    }
    // The derivative's coefficients: quad `2(b - a) + 2t(a - 2b + c)`, cubic `3((b - a) + 2t(a - 2b + c) + t^2(d - a + 3(b - c)))`.
    let (a, b, c) = match ys[..p.len()] {
        [y0, y1, y2] => (0.0, y0 - 2.0 * y1 + y2, y1 - y0),
        [y0, y1, y2, y3] => (y3 - y0 + 3.0 * (y1 - y2), 2.0 * (y0 - 2.0 * y1 + y2), y1 - y0),
        _ => (0.0, 0.0, 0.0),
    };
    let mut add = |t: f64| {
        if t > 0.0 && t < 1.0 && count < 2 && (count == 0 || t > roots[0]) {
            roots[count] = t;
            count += 1;
        }
    };
    if a.abs() > f64::EPSILON {
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let root = disc.sqrt();
            let (r0, r1) = ((-b - root) / (2.0 * a), (-b + root) / (2.0 * a));
            add(r0.min(r1));
            add(r0.max(r1));
        }
    } else if b.abs() > f64::EPSILON {
        add(-c / b);
    }
    // Cut the curve at each root in turn: the piece before it, then the rest, re-parameterised.
    let mut rest = [Point::ZERO; 4];
    rest[..p.len()].copy_from_slice(p);
    let mut taken = 0.0;
    for (n, &t) in roots[..count].iter().enumerate() {
        let (head, tail) = split(&rest[..p.len()], (t - taken) / (1.0 - taken));
        out[n] = head;
        rest = tail;
        taken = t;
    }
    out[count] = rest;
    count + 1
}

/// `p` split at `t` into the part before and the part after, each as a full curve of the same degree.
fn split(p: &[Point], t: f64) -> ([Point; 4], [Point; 4]) {
    let (mut head, mut tail) = ([Point::ZERO; 4], [Point::ZERO; 4]);
    let mut tmp = [Point::ZERO; 4];
    tmp[..p.len()].copy_from_slice(p);
    for level in 0..p.len() {
        head[level] = tmp[0];
        tail[p.len() - 1 - level] = tmp[p.len() - 1 - level];
        for i in 0..p.len() - 1 - level {
            tmp[i] = tmp[i].lerp(tmp[i + 1], t);
        }
    }
    (head, tail)
}

/// `(int)(v * 256)`: Skia's fixed-point curve setup truncates coordinates to 8 fractional bits.
#[allow(clippy::cast_possible_truncation)] // pixel coordinates are far below i64's range
fn fixed8(v: f64) -> i64 {
    (v * 256.0) as i64
}

/// Skia's `cheap_distance`: the larger of the two plus half the smaller.
fn cheap_distance(dx: i64, dy: i64) -> i64 {
    let (dx, dy) = (dx.abs(), dy.abs());
    if dx > dy { dx + (dy >> 1) } else { dy + (dx >> 1) }
}

/// Skia's `diff_to_shift` with the anti-aliasing shift of 2: halvings so the flattening error stays under about 1/8 px.
fn diff_to_shift(dx: i64, dy: i64) -> u32 {
    const SHIFT_AA: i64 = 2;
    let dist = (cheap_distance(dx, dy) + (1 << (2 + SHIFT_AA))) >> (3 + SHIFT_AA);
    (64 - dist.max(0).leading_zeros()) >> 1
}

/// The halvings of a quad's chain (`SkAnalyticQuadraticEdge`): at least 1, at most [`MAX_SHIFT`].
fn quad_shift(p: &[Point]) -> u32 {
    let delta = |a: f64, b: f64, c: f64| ((fixed8(b) << 1) - fixed8(a) - fixed8(c)) >> 2;
    let shift = diff_to_shift(delta(p[0].x, p[1].x, p[2].x), delta(p[0].y, p[1].y, p[2].y));
    shift.clamp(1, MAX_SHIFT)
}

/// The halvings of a cubic's chain (`SkAnalyticCubicEdge`): one more than `diff_to_shift` of the control points' distance
/// from the chord, at most [`MAX_SHIFT`].
fn cubic_shift(p: &[Point]) -> u32 {
    let delta = |a: f64, b: f64, c: f64, d: f64| {
        let (a, b, c, d) = (fixed8(a), fixed8(b), fixed8(c), fixed8(d));
        let one_third = ((a * 8 - b * 15 + 6 * c + d) * 19) >> 9;
        let two_third = ((a + 6 * b - c * 15 + d * 8) * 19) >> 9;
        one_third.abs().max(two_third.abs())
    };
    let shift = diff_to_shift(delta(p[0].x, p[1].x, p[2].x, p[3].x), delta(p[0].y, p[1].y, p[2].y, p[3].y)) + 1;
    shift.min(MAX_SHIFT)
}
