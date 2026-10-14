//! Anti-aliased scan conversion: a port of Skia's analytic coverage ("AAA"), the rasteriser behind the 2D canvas, onto
//! an accumulation-buffer scan converter.
//!
//! What Chrome draws is not the exact area of a shape inside each pixel: Skia snaps the y of every line, curve vertex
//! and edge end to a quarter pixel, replaces a curve by line segments taken from fixed-point forward differencing, and
//! rounds some positions to whole rows. To land on the same pixels this module repeats those steps, then sums the exact
//! signed area of the resulting polygon per pixel by the accumulation-buffer method of `font-rs` (`add_row` is a port of
//! its `draw_line`: each edge adds its trapezoid area to a row of deltas and a prefix sum turns the deltas into coverage). The non-zero rule is
//! `min(|winding area|, 1)`.
//!
//! The steps, each pinned by a comparison against Chrome (`crates/omuzaki-test/tests/raster_probes.rs`):
//! - lines: clipped to the raster first, then both coordinates truncated to 1/256 px and `y` snapped to the nearest 1/4 px
//!   (a vertical edge keeps its exact `x`; a horizontal one lands on a quarter row);
//! - cubics: split at y extrema, `2^shift` segments from Skia's integer forward differencing, the chain started at the
//!   snapped start point and every vertex snapped to a quarter row;
//! - quads: the same chain, but a step longer than two rows is snapped to a whole row and its `x` moved along the segment;
//! - conics (stroke caps and joins): turned into quads within a quarter pixel, as Skia's edge builder does.
//!
//! Everything here is `f32` or integer arithmetic in a fixed order, so the result is bit-identical run to run (ADR 0009).
//! The buffers belong to the [`Scan`] and are reused, so converting a path allocates nothing once they are warm.
//!
//! Port of Skia (`SkAnalyticEdge`, `SkEdge`, `SkScan_AAAPath`) through tiny-skia's `edge.rs`: Copyright 2006 The Android
//! Open Source Project; copyright 2020 Yevhenii Reizner; BSD-3-Clause. This file is a modified port of the accumulation
//! method of font-rs (<https://github.com/raphlinus/font-rs>, Raph Levien; `src/raster.rs`, Copyright 2015 Google Inc.),
//! Apache-2.0. Changes from font-rs (Apache-2.0 §4(b)): clipping to the raster, Skia's edge snapping, and the non-zero rule. The notices are
//! in `raster/NOTICES.md`.

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

use super::geom::{Conic, chop_cubic_at_y_extrema, chop_quad_at_y_extrema};

/// The largest coordinate (in pixels) the fixed-point curve code takes; a curve with a control point beyond it is flattened.
const FIXED_RANGE: f32 = 1.0e6;

/// A forward-differencing step this many rows long (16.16) is snapped to whole rows; a shorter one to quarter rows.
const STEEP: i64 = 2 << 16;

/// Edge vertices of one curve: at most `2^6` segments, so 65 points.
const MAX_CHAIN: usize = 65;

/// The rounded y of Skia's `SnapY` for accuracy 2: nearest quarter pixel, in 16.16 fixed point.
fn snap_y_fixed(y: i64) -> i64 {
    ((y + (1 << 13)) >> 14) << 14
}

/// Skia's `cheap_distance`: max plus half the min.
fn cheap_distance(dx: i64, dy: i64) -> i64 {
    let (dx, dy) = (dx.abs(), dy.abs());
    if dx > dy { dx + (dy >> 1) } else { dy + (dx >> 1) }
}

/// The number of halvings a curve needs so its flattening error stays under about 1/8 px (`diff_to_shift`, with the
/// anti-aliasing shift of 2 Skia always uses here).
fn diff_to_shift(dx: i64, dy: i64) -> i64 {
    const SHIFT_AA: i64 = 2;
    let dist = (cheap_distance(dx, dy) + (1 << (2 + SHIFT_AA))) >> (3 + SHIFT_AA);
    let bits = i64::from(64 - dist.leading_zeros());
    bits >> 1
}

/// `(int)(v * 256)`: the coordinate as a fixed-point number with 8 fractional bits, truncated like Skia's curve setup.
#[allow(clippy::cast_possible_truncation)] // coordinates are far below i64's range
fn fixed8(v: f32) -> i64 {
    (f64::from(v) * 256.0) as i64
}

/// One curve's edge vertices in 16.16 fixed point (`x`, snapped `y`).
struct Chain {
    pts: [(i64, i64); MAX_CHAIN],
    len: usize,
}

impl Chain {
    fn new() -> Self {
        Self { pts: [(0, 0); MAX_CHAIN], len: 0 }
    }

    fn push(&mut self, x: i64, y: i64) {
        self.pts[self.len] = (x, y);
        self.len += 1;
    }

    fn reverse(&mut self) {
        self.pts[..self.len].reverse();
    }
}

/// Whether every control point is a number of a size the fixed-point curve arithmetic can hold.
fn within_fixed_range(points: &[Vec2]) -> bool {
    points.iter().all(|p| p.x.abs() <= FIXED_RANGE && p.y.abs() <= FIXED_RANGE)
}

/// `SkAnalyticQuadraticEdge`'s vertices for a quad monotonic in y.
fn quad_chain(p: [Vec2; 3]) -> Chain {
    let rev = p[0].y > p[2].y;
    let q = if rev { [p[2], p[1], p[0]] } else { p };
    let (x0, x1, x2) = (fixed8(q[0].x), fixed8(q[1].x), fixed8(q[2].x));
    let (y0, y1, y2) = (fixed8(q[0].y), fixed8(q[1].y), fixed8(q[2].y));

    let dx = ((x1 << 1) - x0 - x2) >> 2;
    let dy = ((y1 << 1) - y0 - y2) >> 2;
    let mut shift = diff_to_shift(dx, dy);
    shift = if shift == 0 { 1 } else { shift.min(6) };
    let mut count = 1_i64 << shift;
    let curve_shift = shift - 1;

    // Forward-difference setup in 16.16 (the inputs carry two extra fractional bits, dropped at the end).
    let setup = |a0: i64, a1: i64, a2: i64| {
        let a = (a0 - a1 - a1 + a2) << 9; // half the real coefficient
        let b = (a1 - a0) << 10;
        (a0 << 10, b + (a >> shift), a >> (shift - 1), a2 << 10)
    };
    let (qx, qdx, qddx, last_x) = setup(x0, x1, x2);
    let (qy, qdy, qddy, last_y) = setup(y0, y1, y2);
    let (qx, mut qy, mut qdx, mut qdy) = (qx >> 2, qy >> 2, qdx >> 2, qdy >> 2);
    let (qddx, qddy, last_x, last_y) = (qddx >> 2, qddy >> 2, last_x >> 2, last_y >> 2);
    qy = snap_y_fixed(qy);
    let last_y = snap_y_fixed(last_y);

    let mut chain = Chain::new();
    let (mut snapped_x, mut snapped_y) = (qx, qy);
    chain.push(snapped_x, snapped_y);
    let (mut old_x, mut old_y) = (qx, qy);
    loop {
        count -= 1;
        let (new_x, new_y, new_snapped_x, new_snapped_y);
        if count > 0 {
            new_x = old_x + (qdx >> curve_shift);
            new_y = old_y + (qdy >> curve_shift);
            if (qdy >> curve_shift).abs() >= STEEP {
                let diff_y = (new_y - snapped_y) >> 10;
                let slope = if diff_y == 0 { None } else { Some((((new_x - snapped_x) >> 10) << 16) / diff_y) };
                new_snapped_y = last_y.min(((new_y + 0x8000) >> 16) << 16);
                new_snapped_x = match slope {
                    Some(s) => new_x - ((s * (new_y - new_snapped_y)) >> 16),
                    None => new_x,
                };
            } else {
                new_snapped_y = last_y.min(snap_y_fixed(new_y));
                new_snapped_x = new_x;
            }
            qdx += qddx;
            qdy += qddy;
        } else {
            new_x = last_x;
            new_y = last_y;
            new_snapped_y = new_y;
            new_snapped_x = new_x;
        }
        chain.push(new_snapped_x, new_snapped_y);
        old_x = new_x;
        old_y = new_y;
        snapped_x = new_snapped_x;
        snapped_y = new_snapped_y;
        if count == 0 {
            break;
        }
    }
    if rev {
        chain.reverse();
    }
    chain
}

/// `SkAnalyticCubicEdge`'s vertices for a cubic monotonic in y.
fn cubic_chain(p: [Vec2; 4]) -> Chain {
    let rev = p[0].y > p[3].y;
    let q = if rev { [p[3], p[2], p[1], p[0]] } else { p };
    let x = q.map(|v| fixed8(v.x));
    let y = q.map(|v| fixed8(v.y));

    let delta_from_line = |a: i64, b: i64, c: i64, d: i64| {
        let one_third = ((a * 8 - b * 15 + 6 * c + d) * 19) >> 9;
        let two_third = ((a + 6 * b - c * 15 + d * 8) * 19) >> 9;
        one_third.abs().max(two_third.abs())
    };
    let dx = delta_from_line(x[0], x[1], x[2], x[3]);
    let dy = delta_from_line(y[0], y[1], y[2], y[3]);
    let shift = (diff_to_shift(dx, dy) + 1).min(6);

    let mut up_shift = 6;
    let mut down_shift = shift + up_shift - 10;
    if down_shift < 0 {
        down_shift = 0;
        up_shift = 10 - shift;
    }

    let setup = |a: [i64; 4]| {
        let b = (3 * (a[1] - a[0])) << up_shift;
        let c = (3 * (a[0] - a[1] - a[1] + a[2])) << up_shift;
        let d = (a[3] + 3 * (a[1] - a[2]) - a[0]) << up_shift;
        let cd = b + (c >> shift) + (d >> (2 * shift));
        let cdd = 2 * c + ((3 * d) >> (shift - 1));
        let cddd = (3 * d) >> (shift - 1);
        (a[0] << 10, cd, cdd, cddd, a[3] << 10)
    };
    let (cx, cdx, cddx, cdddx, last_x) = setup(x);
    let (cy, cdy, cddy, cdddy, last_y) = setup(y);
    let (cx, mut cdx, mut cddx, cdddx, last_x) = (cx >> 2, cdx >> 2, cddx >> 2, cdddx >> 2, last_x >> 2);
    let (cy, mut cdy, mut cddy, cdddy, last_y) = (cy >> 2, cdy >> 2, cddy >> 2, cdddy >> 2, last_y >> 2);
    let cy = snap_y_fixed(cy);
    let last_y = snap_y_fixed(last_y);

    let mut chain = Chain::new();
    chain.push(cx, cy);
    let mut count = -(1_i64 << shift);
    let (mut old_x, mut old_y) = (cx, cy);
    loop {
        count += 1;
        let (new_x, mut new_y);
        if count < 0 {
            new_x = old_x + (cdx >> down_shift);
            cdx += cddx >> shift;
            cddx += cdddx;
            new_y = old_y + (cdy >> down_shift);
            cdy += cddy >> shift;
            cddy += cdddy;
        } else {
            new_x = last_x;
            new_y = last_y;
        }
        if new_y < old_y {
            new_y = old_y;
        }
        let new_snapped_y = snap_y_fixed(new_y).min(last_y);
        chain.push(new_x, new_snapped_y);
        old_x = new_x;
        old_y = new_y;
        if count == 0 {
            break;
        }
    }
    if rev {
        chain.reverse();
    }
    chain
}

/// The edge accumulation buffer and its sweep. One per rasteriser; see the module docs.
#[derive(Debug, Default)]
pub(super) struct Scan {
    width: usize,
    height: usize,
    /// Cells per row: the raster width plus two, because an edge at the right border writes two cells.
    stride: usize,
    /// Signed area deltas, all zero between paths (the sweep clears what it reads).
    acc: Vec<f32>,
    /// The rows and columns touched since the last sweep.
    dirty: Dirty,
    /// One row of coverage, handed to the sweep's consumer.
    row: Vec<u8>,
}

#[derive(Copy, Clone, Debug, Default)]
struct Dirty {
    min_y: usize,
    max_y: usize,
    min_x: usize,
    max_x: usize,
}

impl Dirty {
    const NONE: Self = Self { min_y: usize::MAX, max_y: 0, min_x: usize::MAX, max_x: 0 };
}

impl Scan {
    /// Sizes the buffers for a `width` x `height` raster. Allocates only when the size grows. The accumulation buffer is
    /// all zero between paths (the sweep clears what it reads), so a raster of the same size needs no clearing.
    pub fn reset(&mut self, width: usize, height: usize) {
        if (self.width, self.height) != (width, height) {
            self.width = width;
            self.height = height;
            self.stride = width + 2;
            self.acc.clear();
            self.acc.resize(self.stride * height, 0.0);
            self.row.clear();
            self.row.resize(width, 0);
        } else if self.dirty.min_y < self.dirty.max_y {
            // A path was fed and never swept: start clean.
            self.acc.fill(0.0);
        }
        self.dirty = Dirty::NONE;
    }

    /// A straight edge in pixel coordinates: clipped to the raster, then snapped as Skia snaps a line.
    pub fn line(&mut self, a: Vec2, b: Vec2) {
        self.clipped(a, b, true);
    }

    /// A quadratic Bézier (the stroker's offset curves).
    pub fn quad(&mut self, p: [Vec2; 3]) {
        if !within_fixed_range(&p) {
            return self.flattened(&p, 16);
        }
        let (pts, n) = chop_quad_at_y_extrema(p);
        for i in 0..n {
            let piece = [pts[2 * i], pts[2 * i + 1], pts[2 * i + 2]];
            let chain = quad_chain(piece);
            self.chain(&chain);
        }
    }

    /// A cubic Bézier.
    pub fn cubic(&mut self, p: [Vec2; 4]) {
        if !within_fixed_range(&p) {
            return self.flattened(&p, 32);
        }
        let (pts, n) = chop_cubic_at_y_extrema(p);
        for i in 0..n {
            let piece = [pts[3 * i], pts[3 * i + 1], pts[3 * i + 2], pts[3 * i + 3]];
            let chain = cubic_chain(piece);
            self.chain(&chain);
        }
    }

    /// A conic (a rational quadratic), replaced by quads first.
    pub fn conic(&mut self, c: &Conic) {
        let mut quads = [Vec2::ZERO; 33];
        let n = c.quads(&mut quads);
        for i in 0..n {
            self.quad([quads[2 * i], quads[2 * i + 1], quads[2 * i + 2]]);
        }
    }

    /// A curve too large for the fixed-point edge arithmetic (far outside any raster) as `segments` straight edges.
    fn flattened(&mut self, p: &[Vec2], segments: usize) {
        let point = |t: f32| -> Vec2 {
            // De Casteljau on the control polygon.
            let mut tmp = [Vec2::ZERO; 4];
            tmp[..p.len()].copy_from_slice(p);
            for level in (1..p.len()).rev() {
                for i in 0..level {
                    tmp[i] = tmp[i] + (tmp[i + 1] - tmp[i]) * t;
                }
            }
            tmp[0]
        };
        #[allow(clippy::cast_precision_loss)] // at most 32 segments
        let step = |i: usize| point(i as f32 / segments as f32);
        for i in 0..segments {
            self.clipped(step(i), step(i + 1), true);
        }
    }

    fn chain(&mut self, chain: &Chain) {
        #[allow(clippy::cast_precision_loss)] // 16.16 values of a raster coordinate fit f32 to 1/1024 px
        let at = |&(x, y): &(i64, i64)| Vec2::new(x as f32 / 65536.0, y as f32 / 65536.0);
        for pair in chain.pts[..chain.len].windows(2) {
            self.clipped(at(&pair[0]), at(&pair[1]), false);
        }
    }

    /// Clips the edge to the raster (what `SkEdgeClipper` does before an edge is built: cut along the true line at the
    /// top and bottom, and project the parts left or right of the raster onto its border so they keep their winding),
    /// then hands the pieces to [`Scan::edge`]. The arithmetic is in doubles, so coordinates of any finite size clip safely.
    fn clipped(&mut self, a: Vec2, b: Vec2, snap: bool) {
        #[allow(clippy::cast_precision_loss)] // raster sizes are far below 2^24
        let (w, h) = (self.width as f64, self.height as f64);
        // Not a number, fully above or below the raster, or horizontal: no area to add.
        if !(a.is_finite() && b.is_finite()) {
            return;
        }
        let (ax, ay, bx, by) = (f64::from(a.x), f64::from(a.y), f64::from(b.x), f64::from(b.y));
        if (ay <= 0.0 && by <= 0.0) || (ay >= h && by >= h) || ay == by {
            return;
        }
        // Cut at y = 0 and y = h along the true line.
        let dxdy = (bx - ax) / (by - ay);
        let cut_y = |x: f64, y: f64| {
            if y < 0.0 {
                (ax + dxdy * (0.0 - ay), 0.0)
            } else if y > h {
                (ax + dxdy * (h - ay), h)
            } else {
                (x, y)
            }
        };
        let (px, py) = cut_y(ax, ay);
        let (qx, qy) = cut_y(bx, by);
        // Cut at x = 0 and x = w; parts outside become vertical edges on the border.
        let mut cuts = [0.0_f64; 4];
        let mut n = 1;
        if (qx - px).abs() > f64::EPSILON {
            for border in [0.0, w] {
                let t = (border - px) / (qx - px);
                if t > 0.0 && t < 1.0 {
                    cuts[n] = t;
                    n += 1;
                }
            }
        }
        cuts[n] = 1.0;
        n += 1;
        cuts[1..n - 1].sort_by(f64::total_cmp);
        for i in 0..n - 1 {
            let (t0, t1) = (cuts[i], cuts[i + 1]);
            let (s0x, s0y) = (px + (qx - px) * t0, py + (qy - py) * t0);
            let (s1x, s1y) = (px + (qx - px) * t1, py + (qy - py) * t1);
            let mid_x = (s0x + s1x) * 0.5;
            let (s0x, s1x) = if mid_x < 0.0 {
                (0.0, 0.0)
            } else if mid_x > w {
                (w, w)
            } else {
                (s0x.clamp(0.0, w), s1x.clamp(0.0, w))
            };
            #[allow(clippy::cast_possible_truncation)] // clipped to the raster, so far inside f32 range
            self.edge(Vec2::new(s0x as f32, s0y as f32), Vec2::new(s1x as f32, s1y as f32), snap);
        }
    }

    /// Snaps (for lines) and accumulates one clipped edge.
    fn edge(&mut self, a: Vec2, b: Vec2, snap: bool) {
        let (a, b) = if snap { (snap_point(a), snap_point(b)) } else { (a, b) };
        self.accumulate(a, b);
    }

    /// Adds the signed area of one edge (inside the raster, `y` increasing downward) to the accumulation buffer.
    fn accumulate(&mut self, a: Vec2, b: Vec2) {
        if a.y == b.y {
            return;
        }
        let (dir, a, b) = if a.y < b.y { (1.0_f32, a, b) } else { (-1.0, b, a) };
        let dxdy = (b.x - a.x) / (b.y - a.y);
        let mut x = a.x;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clipped: 0 <= y <= height
        let (y_first, y_last) = (a.y.floor() as usize, (b.y.ceil() as usize).min(self.height));
        for y in y_first..y_last {
            #[allow(clippy::cast_precision_loss)] // rows are far below 2^24
            let (top, bottom) = (y as f32, (y + 1) as f32);
            let dy = bottom.min(b.y) - top.max(a.y);
            // The border is a hard limit: float error must not walk an edge out of the buffer.
            #[allow(clippy::cast_precision_loss)] // raster widths are far below 2^24
            let x_next = (x + dxdy * dy).clamp(0.0, self.width as f32);
            let d = dy * dir;
            let (x0, x1) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let row = y * self.stride;
            self.add_row(row, d, x0, x1);
            x = x_next;
        }
        self.dirty.min_y = self.dirty.min_y.min(y_first);
        self.dirty.max_y = self.dirty.max_y.max(y_last);
    }

    /// Spreads `d` (a signed height) over the cells an edge piece crosses in one row, from `x0` to `x1` (x0 <= x1).
    fn add_row(&mut self, row: usize, d: f32, x0: f32, x1: f32) {
        let x0_floor = x0.floor();
        let x1_ceil = x1.ceil();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clipped to 0 <= x <= width
        let (x0i, x1i) = (x0_floor as usize, x1_ceil as usize);
        self.dirty.min_x = self.dirty.min_x.min(x0i);
        self.dirty.max_x = self.dirty.max_x.max(x1i + 1);
        let acc = &mut self.acc[row..row + self.stride];
        if x1i <= x0i + 1 {
            let xmf = 0.5 * (x0 + x1) - x0_floor;
            acc[x0i] += d - d * xmf;
            acc[x0i + 1] += d * xmf;
        } else {
            let s = (x1 - x0).recip();
            let x0f = x0 - x0_floor;
            let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
            let x1f = x1 - x1_ceil + 1.0;
            let am = 0.5 * s * x1f * x1f;
            acc[x0i] += d * a0;
            if x1i == x0i + 2 {
                acc[x0i + 1] += d * (1.0 - a0 - am);
            } else {
                let a1 = s * (1.5 - x0f);
                acc[x0i + 1] += d * (a1 - a0);
                for cell in &mut acc[x0i + 2..x1i - 1] {
                    *cell += d * s;
                }
                #[allow(clippy::cast_precision_loss)] // the span is at most the raster width
                let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                acc[x1i - 1] += d * (1.0 - a2 - am);
            }
            acc[x1i] += d * am;
        }
    }

    /// Turns the accumulated deltas into coverage, row by row, and clears them. `emit(y, x, alpha)` gets each row's
    /// alpha values (0 to 255) from column `x`; rows and runs with no coverage are skipped.
    ///
    /// This is the non-zero resolve, and the clamp sits after the sum, not per edge. Each edge adds a signed area, so
    /// the prefix sum along a row is the winding area that lies left of each pixel. Two edges that share a pixel with
    /// opposite windings therefore cancel: a ring's reversed inner contour removes the outer contour's fringe from the
    /// hole. The magnitude (`abs`) is taken because the sign of the winding carries no coverage, and the clamp then
    /// saturates a pixel that two same-winding layers fully cover.
    ///
    /// Known limit: the sum is over area, so two regions that share a pixel subtract when their windings are opposite,
    /// and add when they are the same. A figure-8's crossing is the first case: its lobes are +1 and -1, so the pixel
    /// gets |a - b| instead of the ideal a + b, a slight under-coverage notch. Skia's analytic AA does the same, so Chrome
    /// parity holds. A partly covered pixel where two same-winding layers overlap adds their partial areas (0.35 + 0.35
    /// = 0.70) instead of taking the single-layer value; whether Chrome does that too is open.
    /// The resolve follows the accumulation of Skia's analytic AA (`SkScan_AAAPath`, through tiny-skia's
    /// `scan/path_aa.rs`, BSD-3-Clause); the parity probes in `omuzaki-test` are the check against Chrome.
    pub fn sweep(&mut self, mut emit: impl FnMut(usize, usize, &[u8])) {
        let dirty = std::mem::replace(&mut self.dirty, Dirty::NONE);
        if dirty.min_y >= dirty.max_y {
            return;
        }
        let mut row_buf = std::mem::take(&mut self.row);
        for y in dirty.min_y..dirty.max_y {
            let cells = &mut self.acc[y * self.stride..(y + 1) * self.stride];
            let last = dirty.max_x.min(self.stride - 1);
            let mut sum = 0.0_f32;
            let mut any = false;
            for x in dirty.min_x..=last {
                sum += cells[x];
                cells[x] = 0.0;
                if x < self.width {
                    let alpha = coverage_to_alpha(sum.abs());
                    row_buf[x] = alpha;
                    any |= alpha != 0;
                }
            }
            if any {
                let end = (last + 1).min(self.width);
                if dirty.min_x < end {
                    emit(y, dirty.min_x, &row_buf[dirty.min_x..end]);
                }
            }
        }
        self.row = row_buf;
    }
}

/// Skia's alpha from a coverage in 0..=1 (area times 256, clamped to 255; a small bias keeps exact quarters exact).
fn coverage_to_alpha(coverage: f32) -> u8 {
    let c = coverage.min(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0 <= c <= 1
    let a = (c * 256.0 + 0.002) as u32;
    a.min(255) as u8
}

/// A line end as Skia's `setLine` has it: both coordinates truncated to 1/256 px (`SkScalarToFDot6(v * 4)`), then y
/// snapped to the nearest 1/4 px.
fn snap_point(p: Vec2) -> Vec2 {
    let x = fixed8(p.x);
    let y = snap_y_fixed(fixed8(p.y) << 8);
    #[allow(clippy::cast_precision_loss)] // 16.16 values of a raster coordinate fit f32 to 1/1024 px
    Vec2::new(x as f32 / 256.0, y as f32 / 65536.0)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const W: usize = 40;
    const H: usize = 30;

    /// The coverage (0..=1, in floats, not rounded to alpha) of what has been fed, per pixel; clears the buffer.
    fn coverage(scan: &mut Scan) -> Vec<f32> {
        let mut out = vec![0.0; W * H];
        let dirty = std::mem::replace(&mut scan.dirty, Dirty::NONE);
        for y in dirty.min_y..dirty.max_y.min(H) {
            let mut sum = 0.0_f32;
            for x in dirty.min_x..=dirty.max_x.min(scan.stride - 1) {
                let cell = &mut scan.acc[y * scan.stride + x];
                sum += *cell;
                *cell = 0.0;
                if x < W {
                    out[y * W + x] = sum.abs().min(1.0);
                }
            }
        }
        out
    }

    /// An axis-aligned rectangle, counter-clockwise on screen, fed edge by edge.
    fn rect(scan: &mut Scan, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) {
        let corners = [Vec2::new(x0, y0), Vec2::new(x1, y0), Vec2::new(x1, y1), Vec2::new(x0, y1)];
        for i in 0..4 {
            scan.line(corners[i], corners[(i + 1) % 4]);
        }
    }

    fn scan() -> Scan {
        let mut scan = Scan::default();
        scan.reset(W, H);
        scan
    }

    /// A column position on the 1/256 px grid the scan converter keeps x on (it truncates to it), and a row position on
    /// the quarter-pixel grid it snaps y to: on these a rectangle's coverage is its area exactly. Off them the edges move
    /// to the nearest grid line, as Skia's do (`vertical_edges_are_exact_and_horizontal_ones_snap` in `raster_unit.rs`).
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // small grid indices
    fn x_on_grid() -> impl Strategy<Value = f32> {
        (0_u32..(W as u32) * 256).prop_map(|n| n as f32 / 256.0)
    }

    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // small grid indices
    fn y_on_grid() -> impl Strategy<Value = f32> {
        (0_u32..(H as u32) * 4).prop_map(|n| n as f32 / 4.0)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 500, failure_persistence: None, ..ProptestConfig::default() })]

        /// The total coverage of a rectangle is its area, to 1e-4 (the float sums of a few thousand pixels).
        #[test]
        fn a_rectangles_coverage_is_its_area(
            xa in x_on_grid(), xb in x_on_grid(), ya in y_on_grid(), yb in y_on_grid(),
        ) {
            let (x0, x1, y0, y1) = (xa.min(xb), xa.max(xb), ya.min(yb), ya.max(yb));
            let mut scan = scan();
            rect(&mut scan, (x0, y0), (x1, y1));
            let total: f64 = coverage(&mut scan).iter().map(|&c| f64::from(c)).sum();
            let area = f64::from(x1 - x0) * f64::from(y1 - y0);
            prop_assert!((total - area).abs() < 1e-4, "{total} vs {area}");
        }

        /// Two rectangles that share an edge cover what their union does, pixel by pixel: no seam, however the shared edge
        /// falls across a pixel (a vertical one) or a quarter row (a horizontal one).
        #[test]
        fn shapes_sharing_an_edge_leave_no_seam(
            xa in x_on_grid(), xm in x_on_grid(), xb in x_on_grid(), ya in y_on_grid(), ym in y_on_grid(), yb in y_on_grid(),
        ) {
            let mut xs = [xa, xm, xb];
            xs.sort_by(f32::total_cmp);
            let mut ys = [ya, ym, yb];
            ys.sort_by(f32::total_cmp);
            let whole = {
                let mut s = scan();
                rect(&mut s, (xs[0], ys[0]), (xs[2], ys[2]));
                coverage(&mut s)
            };
            for (first, second) in [
                (((xs[0], ys[0]), (xs[1], ys[2])), ((xs[1], ys[0]), (xs[2], ys[2]))),
                (((xs[0], ys[0]), (xs[2], ys[1])), ((xs[0], ys[1]), (xs[2], ys[2]))),
            ] {
                let (mut one, mut two) = (scan(), scan());
                rect(&mut one, first.0, first.1);
                rect(&mut two, second.0, second.1);
                let (one, two) = (coverage(&mut one), coverage(&mut two));
                for i in 0..W * H {
                    prop_assert!((one[i] + two[i] - whole[i]).abs() < 1e-4, "pixel {i}: {} + {} vs {}", one[i], two[i], whole[i]);
                }
            }
        }
    }

    /// Feeds a closed polygon, edge by edge, in the order given.
    fn polygon(scan: &mut Scan, points: &[(f32, f32)]) {
        let n = points.len();
        for i in 0..n {
            let (ax, ay) = points[i];
            let (bx, by) = points[(i + 1) % n];
            scan.line(Vec2::new(ax, ay), Vec2::new(bx, by));
        }
    }

    /// The fraction of pixel `(px, py)` inside a triangle, by 256 x 256 point samples. It is an oracle that shares no code
    /// with the scan converter.
    #[allow(clippy::cast_precision_loss)] // sample indices are below 256
    fn triangle_fraction(t: [(f32, f32); 3], px: usize, py: usize) -> f32 {
        let inside = |x: f32, y: f32| {
            let side = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
            let (d0, d1, d2) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
            (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
        };
        let mut hits = 0_u32;
        for j in 0..256 {
            for i in 0..256 {
                let x = px as f32 + (i as f32 + 0.5) / 256.0;
                let y = py as f32 + (j as f32 + 0.5) / 256.0;
                hits += u32::from(inside(x, y));
            }
        }
        hits as f32 / 65536.0
    }

    /// A ring: an outer square, then an inner one given in the opposite order, so the hole is cut by reversed winding. The
    /// outer fringe that lands in the hole is cancelled by the inner contour's edges, and the hole stays empty.
    #[test]
    fn ring_hole_leaves_no_fringe() {
        let outer = [(4.3, 3.7), (30.6, 3.7), (30.6, 25.2), (4.3, 25.2)];
        let inner = [(10.4, 9.8), (10.4, 19.3), (22.9, 19.3), (22.9, 9.8)];
        let mut s = scan();
        polygon(&mut s, &outer);
        polygon(&mut s, &inner);
        let cov = coverage(&mut s);
        for y in 10..=18 {
            for x in 11..=21 {
                assert!(cov[y * W + x] <= 1.0 / 255.0, "hole pixel ({x}, {y}) = {}", cov[y * W + x]);
            }
        }
        // Pixel 10 of row 12 holds the ring left of the hole's edge at x = 10.4. The scan keeps x to 1/256 px (truncated),
        // so the edge sits at 10.3984 and the pixel holds 0.3984 (exactly 102/256).
        assert!((cov[12 * W + 10] - 0.4).abs() < 1.0 / 256.0, "hole edge pixel {}", cov[12 * W + 10]);
        assert!((cov[12 * W + 5] - 1.0).abs() < 1e-4, "band pixel {}", cov[12 * W + 5]);
    }

    /// A self-overlapping contour (the same square traversed twice) has winding 2 in its interior. Non-zero clamps that to
    /// full coverage, so the interior matches one layer; an even-odd rule would give 0. Partly covered edge pixels are not
    /// asserted here: they add (see the known limit on `sweep`), and that is a known limit.
    #[test]
    fn self_overlap_clamps_to_one_not_even_odd() {
        let square = [(5.3, 4.6), (17.8, 4.6), (17.8, 12.9), (5.3, 12.9)];
        let mut single = scan();
        polygon(&mut single, &square);
        let mut twice = scan();
        polygon(&mut twice, &square);
        polygon(&mut twice, &square);
        let (one, two) = (coverage(&mut single), coverage(&mut twice));
        for i in 0..W * H {
            if one[i] == 1.0 {
                assert!((two[i] - 1.0).abs() < 1e-6, "interior pixel {i}: overlap {}", two[i]);
            }
        }
        assert!((two[8 * W + 10] - 1.0).abs() < 1e-6, "overlap interior {}", two[8 * W + 10]);
    }

    /// A figure-8: its two lobes wind +1 and -1 and meet at a crossing. This pins the current behaviour, a known limit and
    /// not the ideal: the crossing pixel gets |left - right| where non-zero coverage would give left + right, the slight
    /// under-coverage notch Skia's analytic AA also has. The oracle measures each lobe on its own.
    #[test]
    fn figure_eight_lobes_cancel_where_they_cross() {
        let figure_eight = [(5.3, 5.25), (15.6, 15.75), (15.6, 5.25), (5.3, 15.75)];
        let left = [(10.45, 10.5), (5.3, 15.75), (5.3, 5.25)];
        let right = [(10.45, 10.5), (15.6, 5.25), (15.6, 15.75)];
        let mut s = scan();
        polygon(&mut s, &figure_eight);
        let cov = coverage(&mut s);
        let (a, b) = (triangle_fraction(left, 10, 10), triangle_fraction(right, 10, 10));
        let crossing = cov[10 * W + 10];
        assert!((crossing - (a - b).abs()).abs() < 0.02, "crossing {crossing} vs |{a} - {b}|");
        assert!(a + b - crossing > 0.2, "the pixel must hold both lobes, so cancellation is what is tested");
        assert!((cov[10 * W + 13] - 1.0).abs() < 1e-4, "right lobe interior {}", cov[10 * W + 13]);
    }
}
