//! Strokes thinner than a pixel. Skia does not outline these: it draws the path as one-pixel-wide anti-aliased lines
//! and scales their coverage down to the stroke's width, which looks like a thin line of the right weight and is much
//! cheaper. The canvas does the same, so this is what a hairline must look like to match it.
//!
//! The line drawing is a port of Skia's `SkScan_Antihair.cpp` through tiny-skia (fixed point throughout: a line is
//! walked a pixel at a time along its major axis and each step splits its coverage between two pixels); the curves
//! are cut into lines the way `SkScan_Hairline.cpp` cuts them.
//!
//! Port of Skia's `SkScan_Antihair.cpp` and `SkScan_Hairline.cpp` (through tiny-skia). Copyright 2006 and 2011 The Android Open Source Project; copyright 2020
//! Yevhenii Reizner. BSD-3-Clause; the notice is in `raster/NOTICES.md`.

// The numerics follow Skia operation for operation: its short names (`a`, `b`, `x0`, `dx`), its exact float comparisons
// (a vertical edge is exactly vertical, a weight is exactly 1) and its midpoint arithmetic are part of what is reproduced.
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::float_cmp,
    clippy::manual_midpoint,
    clippy::neg_cmp_op_on_partial_ord
)]

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cap, PathEl};

use super::geom::chop_cubic_at;
use super::stroke::find_cubic_max_curvature;

const MAX_QUAD_SUBDIVIDE_LEVEL: u32 = 5;
const MAX_CUBIC_SUBDIVIDE_LEVEL: u32 = 9;

/// The coverage a stroke of `width` (in the path's units) gets when drawn under `to_px`, if it counts as a hairline: no
/// more than a pixel across in either direction (Skia's `SkDrawTreatAsHairline`). `None` for a stroke to outline.
pub(super) fn coverage(width: f32, to_px: Affine2) -> Option<f32> {
    // The length of a mapped vector, approximated as the longer component plus half the shorter.
    fn fast_len(v: Vec2) -> f32 {
        let (mut x, mut y) = (v.x.abs(), v.y.abs());
        if x < y {
            std::mem::swap(&mut x, &mut y);
        }
        x + y * 0.5
    }
    let len0 = fast_len(to_px.x_axis * width);
    let len1 = fast_len(to_px.y_axis * width);
    (len0 <= 1.0 && len1 <= 1.0).then_some((len0 + len1) * 0.5)
}

/// What a segment of the path is, for the cap logic.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Verb {
    Move,
    Line,
    Quad,
    Cubic,
    Close,
}

fn verb(el: &PathEl) -> Verb {
    match el {
        PathEl::Move(_) => Verb::Move,
        PathEl::Line(_) => Verb::Line,
        PathEl::Quad(..) => Verb::Quad,
        PathEl::Cubic(..) => Verb::Cubic,
        PathEl::Close => Verb::Close,
    }
}

/// Draws `els` mapped through `to_px` as hairlines. `emit(x, y, alpha)` is called for every pixel the lines touch,
/// possibly outside the raster (`width` x `height`): it must ignore those. Overlapping segments are emitted one after the
/// other, so they blend, as they do on the canvas.
pub(super) fn stroke(
    els: &[PathEl],
    to_px: Affine2,
    cap: Cap,
    size: (usize, usize),
    emit: &mut impl FnMut(i32, i32, u8),
) {
    let mut lines = Lines { size, emit };
    let at = |p: Vec2| to_px.transform_point2(p);
    let mut prev_verb = Verb::Move;
    let (mut first_pt, mut last_pt) = (Vec2::ZERO, Vec2::ZERO);
    for (i, el) in els.iter().enumerate() {
        let next_verb = els.get(i + 1).map(verb);
        let last_pt2;
        match *el {
            PathEl::Move(p) => {
                first_pt = at(p);
                last_pt = first_pt;
                last_pt2 = first_pt;
            }
            PathEl::Line(p) => {
                let mut points = [last_pt, at(p)];
                if cap != Cap::Butt {
                    extend_pts(prev_verb, next_verb, &mut points);
                }
                lines.polyline(&points);
                last_pt = at(p);
                last_pt2 = points[0];
            }
            PathEl::Quad(c, p) => {
                let mut points = [last_pt, at(c), at(p)];
                if cap != Cap::Butt {
                    extend_pts(prev_verb, next_verb, &mut points);
                }
                lines.quad(&points);
                last_pt = at(p);
                last_pt2 = points[0];
            }
            PathEl::Cubic(a, b, p) => {
                let mut points = [last_pt, at(a), at(b), at(p)];
                if cap != Cap::Butt {
                    extend_pts(prev_verb, next_verb, &mut points);
                }
                lines.cubic(&points);
                last_pt = at(p);
                last_pt2 = points[0];
            }
            PathEl::Close => {
                let mut points = [last_pt, first_pt];
                if cap != Cap::Butt && prev_verb == Verb::Move {
                    // A move then a close is capped, to match what SVG expects of degenerate segments.
                    extend_pts(prev_verb, next_verb, &mut points);
                }
                lines.polyline(&points);
                last_pt2 = points[0];
            }
        }
        if cap != Cap::Butt {
            if prev_verb == Verb::Move && matches!(verb(el), Verb::Line | Verb::Quad | Verb::Cubic) {
                first_pt = last_pt2; // the curve moved the initial point, so close to it instead
            }
            prev_verb = verb(el);
        }
    }
}

/// Extends the points by a round cap's worth along the start and end tangents (`pi / 8`: the cap's area spread over a
/// unit-wide line). A tangent of zero length uses the next control point; a fully degenerate curve moves out half a unit.
fn extend_pts(prev_verb: Verb, next_verb: Option<Verb>, points: &mut [Vec2]) {
    let cap_outset = std::f32::consts::PI / 8.0;
    let n = points.len();
    if prev_verb == Verb::Move {
        let first = points[0];
        let mut offset = 0;
        let mut controls = n - 1;
        let mut tangent;
        loop {
            offset += 1;
            tangent = first - points[offset];
            if tangent != Vec2::ZERO {
                break;
            }
            controls -= 1;
            if controls == 0 {
                break;
            }
        }
        if tangent == Vec2::ZERO {
            tangent = Vec2::new(1.0, 0.0);
            controls = n - 1; // all points equal: move all but one
        } else {
            tangent = normalize(tangent);
        }
        offset = 0;
        loop {
            // If the end point and the control points are equal, move them in tandem.
            points[offset] += tangent * cap_outset;
            offset += 1;
            controls += 1;
            if controls >= n {
                break;
            }
        }
    }

    if matches!(next_verb, Some(Verb::Move | Verb::Close) | None) {
        let last = points[n - 1];
        let mut offset = n - 1;
        let mut controls = n - 1;
        let mut tangent;
        loop {
            offset -= 1;
            tangent = last - points[offset];
            if tangent != Vec2::ZERO {
                break;
            }
            controls -= 1;
            if controls == 0 {
                break;
            }
        }
        if tangent == Vec2::ZERO {
            tangent = Vec2::new(-1.0, 0.0);
            controls = n - 1;
        } else {
            tangent = normalize(tangent);
        }
        offset = n - 1;
        loop {
            points[offset] += tangent * cap_outset;
            // `offset` reaches 0 only when the loop ends.
            if offset == 0 {
                break;
            }
            offset -= 1;
            controls += 1;
            if controls >= n {
                break;
            }
        }
    }
}

/// Skia's `normalize`: scales in doubles so a huge vector cannot overflow.
fn normalize(v: Vec2) -> Vec2 {
    let (x, y) = (f64::from(v.x), f64::from(v.y));
    let scale = 1.0 / (x * x + y * y).sqrt();
    #[allow(clippy::cast_possible_truncation)] // Skia narrows the scale to f32 as well
    let scale = scale as f32;
    Vec2::new(v.x * scale, v.y * scale)
}

struct Lines<'a, F: FnMut(i32, i32, u8)> {
    size: (usize, usize),
    emit: &'a mut F,
}

impl<F: FnMut(i32, i32, u8)> Lines<'_, F> {
    /// A quad as `2^level` lines, the level chosen so the curve is within a pixel of its chord.
    fn quad(&mut self, p: &[Vec2; 3]) {
        let level = quad_level(p);
        let lines = 1_usize << level;
        let c = p[0];
        let b = (p[1] - c) * 2.0;
        let a = p[2] - p[1] * 2.0 + c;
        let mut tmp = [Vec2::ZERO; (1 << MAX_QUAD_SUBDIVIDE_LEVEL) + 1];
        tmp[0] = p[0];
        let mut t = Vec2::ZERO;
        #[allow(clippy::cast_precision_loss)] // at most 32 lines
        let dt = Vec2::splat(1.0 / lines as f32);
        for point in tmp.iter_mut().take(lines).skip(1) {
            t += dt;
            *point = (a * t + b) * t + c;
        }
        tmp[lines] = p[2];
        self.polyline(&tmp[..=lines]);
    }

    fn cubic(&mut self, p: &[Vec2; 4]) {
        if is_nice_cubic(p) {
            self.cubic_lines(p);
        } else {
            // Cut at the points of maximum curvature, where lines would stray from the curve.
            let mut roots = [0.0_f32; 3];
            let n = find_cubic_max_curvature(p, &mut roots);
            let mut ts = [0.0_f32; 2];
            let mut count = 0;
            for &t in &roots[..n] {
                if t > 0.0 && t < 1.0 && count < 2 {
                    ts[count] = t;
                    count += 1;
                }
            }
            let pieces = chop_cubic_at(*p, &ts[..count]);
            for i in 0..=count {
                self.cubic_lines(&[pieces[3 * i], pieces[3 * i + 1], pieces[3 * i + 2], pieces[3 * i + 3]]);
            }
        }
    }

    fn cubic_lines(&mut self, p: &[Vec2; 4]) {
        let lines = cubic_segments(p);
        if lines == 1 {
            self.polyline(&[p[0], p[3]]);
            return;
        }
        let a = p[3] + (p[1] - p[2]) * 3.0 - p[0];
        let b = (p[2] - p[1] * 2.0 + p[0]) * 3.0;
        let c = (p[1] - p[0]) * 3.0;
        let d = p[0];
        let mut tmp = [Vec2::ZERO; (1 << MAX_CUBIC_SUBDIVIDE_LEVEL) + 1];
        tmp[0] = p[0];
        let mut t = Vec2::ZERO;
        #[allow(clippy::cast_precision_loss)] // at most 512 lines
        let dt = Vec2::splat(1.0 / lines as f32);
        for point in tmp.iter_mut().take(lines).skip(1) {
            t += dt;
            *point = ((a * t + b) * t + c) * t + d;
        }
        if tmp[..lines].iter().all(|q| q.is_finite()) {
            tmp[lines] = p[3];
            self.polyline(&tmp[..=lines]);
        }
    }

    /// Draws the segments between consecutive points, each clipped to the raster outset by a pixel (an anti-aliased
    /// hairline reaches half a pixel beyond its ends).
    fn polyline(&mut self, points: &[Vec2]) {
        #[allow(clippy::cast_precision_loss)] // raster sizes are far below 2^24
        let (w, h) = (self.size.0 as f32 + 1.0, self.size.1 as f32 + 1.0);
        for pair in points.windows(2) {
            let Some((a, b)) = clip_line(pair[0], pair[1], Vec2::splat(-1.0), Vec2::new(w, h)) else { continue };
            self.line(to_fdot6(a.x), to_fdot6(a.y), to_fdot6(b.x), to_fdot6(b.y));
        }
    }
}

/// A coordinate in 26.6 fixed point, truncated and saturating like Skia's `fdot6::from_f32`.
#[allow(clippy::cast_possible_truncation)] // saturates: the line was clipped to the raster first
fn to_fdot6(v: f32) -> i32 {
    (f64::from(v) * 64.0) as i32
}

/// The part of the segment inside the box, or `None` if there is none.
fn clip_line(a: Vec2, b: Vec2, min: Vec2, max: Vec2) -> Option<(Vec2, Vec2)> {
    if !(a.is_finite() && b.is_finite()) {
        return None;
    }
    let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
    let d = b - a;
    for (p, q) in [(-d.x, a.x - min.x), (d.x, max.x - a.x), (-d.y, a.y - min.y), (d.y, max.y - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return None;
            }
        }
    }
    Some((a + d * t0, a + d * t1))
}

/// How many halvings bring a quad within a pixel of its chord (Skia's `compute_quad_level`).
fn quad_level(p: &[Vec2; 3]) -> u32 {
    // The vector from the control point to the middle of the chord, in whole pixels (rounded up).
    let dx = ((p[0].x + p[2].x) * 0.5 - p[1].x).abs();
    let dy = ((p[0].y + p[2].y) * 0.5 - p[1].y).abs();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // saturating like Skia's `saturate_from`
    let (idx, idy) = (dx.ceil().min(2_147_483_647.0) as u32, dy.ceil().min(2_147_483_647.0) as u32);
    let d = if idx > idy { idx + (idy >> 1) } else { idy + (idx >> 1) };
    // A quad approaches its chord four times closer with each halving.
    ((33 - d.leading_zeros()) >> 1).min(MAX_QUAD_SUBDIVIDE_LEVEL)
}

/// Whether both off-curve points are inside the limits of the on-curve ones (no sharp turn to straddle).
fn is_nice_cubic(p: &[Vec2; 4]) -> bool {
    let lt_90 = |p0: Vec2, pivot: Vec2, p2: Vec2| (p0 - pivot).dot(p2 - pivot) >= 0.0;
    lt_90(p[1], p[0], p[3]) && lt_90(p[2], p[0], p[3]) && lt_90(p[1], p[3], p[0]) && lt_90(p[2], p[3], p[0])
}

/// How many lines a cubic needs: its control points' distance from the chord's thirds, in steps of four.
fn cubic_segments(p: &[Vec2; 4]) -> usize {
    let p13 = p[3] / 3.0 + p[0] * (2.0 / 3.0);
    let p23 = p[0] / 3.0 + p[3] * (2.0 / 3.0);
    let (d1, d2) = ((p[1] - p13).abs(), (p[2] - p23).abs());
    let diff = d1.max(d2).max_element();
    let mut tolerance = 1.0 / 8.0;
    for i in 0..MAX_CUBIC_SUBDIVIDE_LEVEL {
        if diff < tolerance {
            return 1 << i;
        }
        tolerance *= 4.0;
    }
    1 << MAX_CUBIC_SUBDIVIDE_LEVEL
}

// ---------------------------------------------------------------------------------------- the anti-aliased line

/// Multiplies `value` by `scale` in 0..=64 and shifts down 6 (Skia's `fdot6::small_scale`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // at most 255 * 64 >> 6
fn small_scale(value: u8, scale: i32) -> u8 {
    ((i32::from(value) * scale) >> 6) as u8
}

/// The low 8 bits of a 16.16 number, as an alpha.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // masked to 8 bits
fn i32_to_alpha(a: i32) -> u8 {
    (a & 0xFF) as u8
}

/// The four ways a line is walked: horizontal, mostly horizontal, vertical, mostly vertical.
const HLINE: u8 = 0;
const HORISH: u8 = 1;
const VLINE: u8 = 2;
const VERTISH: u8 = 3;

const ONE: i32 = 1 << 16;
const HALF: i32 = ONE / 2;

impl<F: FnMut(i32, i32, u8)> Lines<'_, F> {
    fn pixel(&mut self, x: i32, y: i32, alpha: u8) {
        if alpha != 0 {
            (self.emit)(x, y, alpha);
        }
    }

    fn hline(&mut self, x: i32, y: i32, count: i32, alpha: u8) {
        for i in 0..count {
            self.pixel(x + i, y, alpha);
        }
    }

    fn vline(&mut self, x: i32, y: i32, count: i32, alpha: u8) {
        for i in 0..count {
            self.pixel(x, y + i, alpha);
        }
    }

    /// One line, in 26.6 fixed point, drawn a pixel at a time along its major axis.
    #[allow(clippy::too_many_lines)] // the four cases of Skia's `do_anti_hairline`, kept together
    fn line(&mut self, mut x0: i32, mut y0: i32, mut x1: i32, mut y1: i32) {
        // A long line is drawn in halves so no intermediate overflows (the halves are shifted separately).
        if (x1 - x0).abs() > 511 << 6 || (y1 - y0).abs() > 511 << 6 {
            let hx = (x0 >> 1) + (x1 >> 1);
            let hy = (y0 >> 1) + (y1 >> 1);
            self.line(x0, y0, hx, hy);
            self.line(hx, hy, x1, y1);
            return;
        }

        let (scale_start, scale_stop, istart, istop, mut fstart, slope, kind);
        if (x1 - x0).abs() > (y1 - y0).abs() {
            // Mostly horizontal: go left to right.
            if x0 > x1 {
                std::mem::swap(&mut x0, &mut x1);
                std::mem::swap(&mut y0, &mut y1);
            }
            istart = x0 >> 6;
            istop = (x1 + 63) >> 6;
            fstart = y0 << 10;
            if y0 == y1 {
                slope = 0;
                kind = HLINE;
            } else {
                slope = ((y1 - y0) << 16) / (x1 - x0);
                fstart += (slope * (32 - (x0 & 63)) + 32) >> 6;
                kind = HORISH;
            }
            if istop - istart == 1 {
                // Within a single pixel.
                scale_start = x1 - x0;
                scale_stop = 0;
            } else {
                scale_start = 64 - (x0 & 63);
                scale_stop = x1 & 63;
            }
        } else {
            // Mostly vertical: go top to bottom.
            if y0 > y1 {
                std::mem::swap(&mut x0, &mut x1);
                std::mem::swap(&mut y0, &mut y1);
            }
            istart = y0 >> 6;
            istop = (y1 + 63) >> 6;
            fstart = x0 << 10;
            if x0 == x1 {
                if y0 == y1 {
                    return; // zero length
                }
                slope = 0;
                kind = VLINE;
            } else {
                slope = ((x1 - x0) << 16) / (y1 - y0);
                fstart += (slope * (32 - (y0 & 63)) + 32) >> 6;
                kind = VERTISH;
            }
            if istop - istart == 1 {
                scale_start = y1 - y0;
                scale_stop = 0;
            } else {
                scale_start = 64 - (y0 & 63);
                scale_stop = y1 & 63;
            }
        }
        // A zero-extent line has no pixels.
        if istop <= istart {
            return;
        }

        let mut istart = istart;
        fstart = self.draw_cap(kind, istart, fstart, slope, scale_start);
        istart += 1;
        let full_spans = istop - istart - i32::from(scale_stop > 0);
        if full_spans > 0 {
            fstart = self.draw_line(kind, istart, istart + full_spans, fstart, slope);
        }
        if scale_stop > 0 {
            self.draw_cap(kind, istop - 1, fstart, slope, scale_stop);
        }
    }

    /// The first or last pixel step of a line, whose coverage is scaled by `mod64` (the part of the pixel the line covers).
    /// Returns the minor-axis position for the next step.
    fn draw_cap(&mut self, kind: u8, along: i32, mut f: i32, slope: i32, mod64: i32) -> i32 {
        f += HALF;
        f = f.max(0);
        match kind {
            // Horizontal line: two pixels in a column, `y` from the position.
            HLINE => {
                let y = f >> 16;
                let a = i32_to_alpha(f >> 8);
                self.pixel(along, y, small_scale(a, mod64));
                self.pixel(along, y - 1, small_scale(255 - a, mod64));
                f - HALF
            }
            // Mostly horizontal: the same, and the position moves along the slope.
            HORISH => {
                let lower_y = f >> 16;
                let a = i32_to_alpha(f >> 8);
                self.pixel(along, lower_y.max(1) - 1, small_scale(255 - a, mod64));
                self.pixel(along, lower_y.max(1), small_scale(a, mod64));
                f + slope - HALF
            }
            // Vertical line: two pixels in a row.
            VLINE => {
                let x = f >> 16;
                let a = i32_to_alpha(f >> 8);
                self.pixel(x, along, small_scale(a, mod64));
                self.pixel(x.max(1) - 1, along, small_scale(255 - a, mod64));
                f - HALF
            }
            _ => {
                let x = f >> 16;
                let a = i32_to_alpha(f >> 8);
                self.pixel(x.max(1) - 1, along, small_scale(255 - a, mod64));
                self.pixel(x.max(1), along, small_scale(a, mod64));
                f + slope - HALF
            }
        }
    }

    /// The pixels from `along` to `stop` (exclusive), each at full scale. Returns the minor-axis position after them.
    fn draw_line(&mut self, kind: u8, along: i32, stop: i32, mut f: i32, slope: i32) -> i32 {
        match kind {
            HLINE => {
                let count = stop - along;
                if count <= 0 {
                    return f;
                }
                f += HALF;
                f = f.max(0);
                let y = f >> 16;
                let a = i32_to_alpha(f >> 8);
                self.hline(along, y, count, a);
                self.hline(along, y - 1, count, 255 - a);
                f - HALF
            }
            HORISH => {
                f += HALF;
                let mut x = along;
                loop {
                    f = f.max(0);
                    let lower_y = f >> 16;
                    let a = i32_to_alpha(f >> 8);
                    self.pixel(x, lower_y.max(1) - 1, 255 - a);
                    self.pixel(x, lower_y.max(1), a);
                    f += slope;
                    x += 1;
                    if x >= stop {
                        break;
                    }
                }
                f - HALF
            }
            VLINE => {
                let count = stop - along;
                if count <= 0 {
                    return f;
                }
                f += HALF;
                f = f.max(0);
                let x = f >> 16;
                let a = i32_to_alpha(f >> 8);
                self.vline(x, along, count, a);
                self.vline(x.max(1) - 1, along, count, 255 - a);
                f - HALF
            }
            _ => {
                f += HALF;
                let mut y = along;
                loop {
                    f = f.max(0);
                    let x = f >> 16;
                    let a = i32_to_alpha(f >> 8);
                    self.pixel(x.max(1) - 1, y, 255 - a);
                    self.pixel(x.max(1), y, a);
                    f += slope;
                    y += 1;
                    if y >= stop {
                        break;
                    }
                }
                f - HALF
            }
        }
    }
}
