//! The stroker: turns a path and a line width into the outline a fill draws (spec CB-24).
//!
//! This is a port of Skia's `SkStroke.cpp`, taken through tiny-skia's `PathStroker`, kept operation for operation. It
//! is split by concern: `segments` (walking the path), `curves` (measuring and classifying curves), `joins`, `caps`. The canvas
//! strokes with this algorithm, and it is not an exact offset: it approximates each offset curve by quads that are only
//! within a quarter pixel of true, so a different stroker would put the edges of a curved stroke in different places.
//! What is left out is what a face drawing cannot ask for: dashes, square caps, bevel and clipped-miter joins.
//!
//! The working buffers (`inner`, `outer`, `cusper`) live in the [`Stroker`] and keep their memory between strokes, so
//! stroking allocates nothing once they have grown to the size of the largest path.
//!
//! Port of Skia (through tiny-skia). Copyright 2008 The Android Open Source Project; copyright 2020 Yevhenii
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

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cap, Join, PathEl};

use super::path::Outline;

mod caps;
mod curves;
mod joins;
mod segments;
#[cfg(test)]
mod tests;

use caps::{cap, push_circle};
pub(super) use curves::find_cubic_max_curvature;
use curves::{
    Reduction, check_cubic_linear, check_quad_linear, cross, degenerate_vector, eval_cubic_pos_at,
    eval_cubic_tangent_at, eval_quad_at, eval_quad_tangent_at, find_cubic_cusp, find_cubic_inflections,
    intersect_quad_ray, is_nearly_zero, points_within_dist, pt_in_quad_bounds, pt_to_line, sharp_angle, with_length,
};
use joins::{Builders, miter_joiner, round_joiner, set_normal_unit_normal, set_normal_unit_normal2};
use segments::{Segment, Segments};

pub(super) const NEARLY_ZERO: f32 = 1.0 / 4096.0;
pub(super) const ROOT_2_OVER_2: f32 = std::f32::consts::FRAC_1_SQRT_2;
pub(super) const PI: f32 = std::f32::consts::PI;
/// The conics of a unit circle: a quadrant each, as control polygons in order.
pub(super) const QUADRANT_POINTS: [Vec2; 8] = [
    Vec2::new(1.0, 0.0),
    Vec2::new(1.0, 1.0),
    Vec2::new(0.0, 1.0),
    Vec2::new(-1.0, 1.0),
    Vec2::new(-1.0, 0.0),
    Vec2::new(-1.0, -1.0),
    Vec2::new(0.0, -1.0),
    Vec2::new(1.0, -1.0),
];

/// Recursion limits per stroke kind: cubics without and with tangents found, then quads (3x those seen in practice).
const RECURSIVE_LIMITS: [i32; 4] = [5 * 3, 26 * 3, 11 * 3, 11 * 3];
const QUAD_RECURSIVE_LIMIT: usize = 3;
/// The subdivisions one stroke may make across all its curves: far more than a face drawing needs (a long curve takes a
/// few dozen), few enough that a pathological one cannot stall the frame.
const SUBDIVISION_BUDGET: u32 = 50_000;

#[derive(Copy, Clone, PartialEq, Debug, Default)]
enum StrokeType {
    #[default]
    Outer = 1,
    Inner = -1,
}

#[derive(Copy, Clone, PartialEq, Debug)]
enum ResultType {
    /// The caller should split the quad stroke in two.
    Split,
    /// The caller should add a line.
    Degenerate,
    /// The caller should (continue to try to) add a quad stroke.
    Quad,
}

#[derive(Copy, Clone, PartialEq, Debug)]
enum IntersectRay {
    CtrlPt,
    ResultType,
}

#[derive(Copy, Clone, PartialEq, Debug, Default)]
enum JoinKind {
    #[default]
    Miter,
    Round,
}

/// The state of the quad stroke under construction.
#[derive(Copy, Clone, Debug)]
struct QuadConstruct {
    /// The stroked quad parallel to the original curve.
    quad: [Vec2; 3],
    tangent_start: Vec2,
    tangent_end: Vec2,
    start_t: f32,
    mid_t: f32,
    end_t: f32,
    start_set: bool,
    end_set: bool,
    /// Set if coincident tangents have opposite directions.
    opposite_tangents: bool,
}

impl QuadConstruct {
    fn new() -> Self {
        Self {
            quad: [Vec2::ZERO; 3],
            tangent_start: Vec2::ZERO,
            tangent_end: Vec2::ZERO,
            start_t: 0.0,
            mid_t: 0.0,
            end_t: 0.0,
            start_set: false,
            end_set: false,
            opposite_tangents: false,
        }
    }

    /// False if start and end are too close to have a unique middle.
    fn init(&mut self, start: f32, end: f32) -> bool {
        self.start_t = start;
        self.mid_t = ((start + end) * 0.5).clamp(0.0, 1.0);
        self.end_t = end;
        self.start_set = false;
        self.end_set = false;
        self.start_t < self.mid_t && self.mid_t < self.end_t
    }

    fn init_with_start(&mut self, parent: &Self) -> bool {
        if !self.init(parent.start_t, parent.mid_t) {
            return false;
        }
        self.quad[0] = parent.quad[0];
        self.tangent_start = parent.tangent_start;
        self.start_set = true;
        true
    }

    fn init_with_end(&mut self, parent: &Self) -> bool {
        if !self.init(parent.mid_t, parent.end_t) {
            return false;
        }
        self.quad[2] = parent.quad[2];
        self.tangent_end = parent.tangent_end;
        self.end_set = true;
        true
    }
}

/// The path stroker. Reuse one: its buffers keep their memory.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // Skia's stroker state, kept as it is
pub(super) struct Stroker {
    radius: f32,
    inv_miter_limit: f32,
    res_scale: f32,
    inv_res_scale: f32,
    inv_res_scale_squared: f32,

    first_normal: Vec2,
    prev_normal: Vec2,
    first_unit_normal: Vec2,
    prev_unit_normal: Vec2,

    /// On the original path.
    first_pt: Vec2,
    prev_pt: Vec2,

    first_outer_pt: Vec2,
    first_outer_pt_index_in_contour: usize,
    segment_count: i32,
    prev_is_line: bool,

    round_caps: bool,
    joiner: JoinKind,

    /// `outer` is the answer, `inner` a temporary for the contour being walked.
    inner: Outline,
    outer: Outline,
    cusper: Outline,

    stroke_type: StrokeType,

    recursion_depth: i32,
    found_tangents: bool,
    join_completed: bool,
    /// Offset-curve subdivisions left for this stroke: a bound on the work a pathological curve can cost.
    budget: u32,
}

impl Stroker {
    /// The resolution scale Skia strokes with: how many pixels a unit of the path covers at most.
    pub fn resolution_scale(to_px: Affine2) -> f32 {
        let (sx, sy) = (to_px.x_axis.length(), to_px.y_axis.length());
        if sx.is_finite() && sy.is_finite() {
            let scale = sx.max(sy);
            if scale > 0.0 {
                return scale;
            }
        }
        1.0
    }

    /// The outline of the last stroke: closed contours of lines, quads and conics, ready to fill with the non-zero rule.
    pub fn outline(&self) -> &Outline {
        &self.outer
    }

    /// Strokes `els` with `width` into [`Stroker::outline`]. `res_scale` is [`Stroker::resolution_scale`] of the
    /// transform the outline will be drawn under; `miter_limit` is the canvas's.
    pub fn stroke(&mut self, els: &[PathEl], width: f32, cap: Cap, join: Join, miter_limit: f32, res_scale: f32) {
        self.reset(width, cap, join, miter_limit, res_scale);
        let mut last_segment_is_line = false;
        let mut iter = Segments::new(els);
        while let Some(segment) = iter.next() {
            match segment {
                Segment::Move(p) => self.move_to(p),
                Segment::Line(p) => {
                    self.line_to(p, Some(&iter));
                    last_segment_is_line = true;
                }
                Segment::Quad(c, p) => {
                    self.quad_to(c, p);
                    last_segment_is_line = false;
                }
                Segment::Cubic(a, b, p) => {
                    self.cubic_to(a, b, p);
                    last_segment_is_line = false;
                }
                Segment::Close => {
                    if self.round_caps {
                        // A move followed by a close is a zero-length line, which a round cap still draws.
                        if self.segment_count == 0 {
                            self.line_to(self.first_pt, None);
                            last_segment_is_line = true;
                            continue;
                        }
                        // The same for a move followed by zero-length segments and a close.
                        if self.is_current_contour_empty() {
                            last_segment_is_line = true;
                            continue;
                        }
                    }
                    self.finish_contour(true, last_segment_is_line);
                }
            }
        }
        self.finish_contour(false, last_segment_is_line);
    }

    fn reset(&mut self, width: f32, cap: Cap, join: Join, miter_limit: f32, res_scale: f32) {
        let join_kind = match join {
            Join::Miter => JoinKind::Miter,
            Join::Round => JoinKind::Round,
        };
        let mut inv_miter_limit = 0.0;
        if join_kind == JoinKind::Miter {
            if miter_limit <= 1.0 {
                // A limit of 1 or less bevels every corner; the miter joiner does that when its ratio is infinite.
                inv_miter_limit = f32::INFINITY;
            } else {
                inv_miter_limit = miter_limit.recip();
            }
        }
        self.res_scale = res_scale;
        // The 4 matches the fill scan converter's error term.
        self.inv_res_scale = (res_scale * 4.0).recip();
        self.inv_res_scale_squared = self.inv_res_scale * self.inv_res_scale;
        self.radius = width * 0.5;
        self.inv_miter_limit = inv_miter_limit;
        self.first_normal = Vec2::ZERO;
        self.prev_normal = Vec2::ZERO;
        self.first_unit_normal = Vec2::ZERO;
        self.prev_unit_normal = Vec2::ZERO;
        self.first_pt = Vec2::ZERO;
        self.prev_pt = Vec2::ZERO;
        self.first_outer_pt = Vec2::ZERO;
        self.first_outer_pt_index_in_contour = 0;
        self.segment_count = -1;
        self.prev_is_line = false;
        self.round_caps = cap == Cap::Round;
        self.joiner = join_kind;
        self.inner.clear();
        self.outer.clear();
        self.cusper.clear();
        self.stroke_type = StrokeType::Outer;
        self.recursion_depth = 0;
        self.found_tangents = false;
        self.join_completed = false;
        self.budget = SUBDIVISION_BUDGET;
    }

    /// Spends one subdivision; true when none are left.
    fn spend(&mut self) -> bool {
        self.budget = self.budget.saturating_sub(1);
        self.budget == 0
    }

    fn move_to(&mut self, p: Vec2) {
        if self.segment_count > 0 {
            self.finish_contour(false, false);
        }
        self.segment_count = 0;
        self.first_pt = p;
        self.prev_pt = p;
        self.join_completed = false;
    }

    fn line_to(&mut self, p: Vec2, iter: Option<&Segments<'_>>) {
        let tolerance = NEARLY_ZERO * self.inv_res_scale;
        let teeny_line = (self.prev_pt.x - p.x).abs() <= tolerance && (self.prev_pt.y - p.y).abs() <= tolerance;
        if !self.round_caps && teeny_line {
            return;
        }
        if teeny_line && (self.join_completed || iter.is_some_and(Segments::has_valid_tangent)) {
            return;
        }
        let Some((normal, unit_normal)) = self.pre_join_to(p, true) else { return };
        self.outer.line_to(p + normal);
        self.inner.line_to(p - normal);
        self.post_join_to(p, normal, unit_normal);
    }

    fn quad_to(&mut self, p1: Vec2, p2: Vec2) {
        let quad = [self.prev_pt, p1, p2];
        let (reduction, kind) = check_quad_linear(&quad);
        match kind {
            Reduction::Point | Reduction::Line => {
                // A move followed by a degenerate curve is a zero-length line, which round caps draw.
                self.line_to(p2, None);
                return;
            }
            Reduction::Degenerate => {
                self.line_to(reduction, None);
                let saved = self.joiner;
                self.joiner = JoinKind::Round;
                self.line_to(p2, None);
                self.joiner = saved;
                return;
            }
            _ => {}
        }

        let Some((normal_ab, unit_ab)) = self.pre_join_to(p1, false) else {
            self.line_to(p2, None);
            return;
        };
        let mut quad_points = QuadConstruct::new();
        self.init_quad(StrokeType::Outer, 0.0, 1.0, &mut quad_points);
        self.quad_stroke(&quad, &mut quad_points);
        self.init_quad(StrokeType::Inner, 0.0, 1.0, &mut quad_points);
        self.quad_stroke(&quad, &mut quad_points);

        let (normal_bc, unit_bc) =
            set_normal_unit_normal(quad[1], quad[2], self.res_scale, self.radius).unwrap_or((normal_ab, unit_ab));
        self.post_join_to(p2, normal_bc, unit_bc);
    }

    fn cubic_to(&mut self, pt1: Vec2, pt2: Vec2, pt3: Vec2) {
        let cubic = [self.prev_pt, pt1, pt2, pt3];
        let mut reduction = [Vec2::ZERO; 3];
        let (kind, tangent_pt) = check_cubic_linear(&cubic, &mut reduction);
        match kind {
            Reduction::Point | Reduction::Line => {
                self.line_to(pt3, None);
                return;
            }
            Reduction::Degenerate | Reduction::Degenerate2 | Reduction::Degenerate3 => {
                self.line_to(reduction[0], None);
                let saved = self.joiner;
                self.joiner = JoinKind::Round;
                if kind >= Reduction::Degenerate2 {
                    self.line_to(reduction[1], None);
                }
                if kind == Reduction::Degenerate3 {
                    self.line_to(reduction[2], None);
                }
                self.line_to(pt3, None);
                self.joiner = saved;
                return;
            }
            Reduction::Quad => {}
        }

        let Some((normal_ab, unit_ab)) = self.pre_join_to(tangent_pt, false) else {
            self.line_to(pt3, None);
            return;
        };

        let mut roots = [0.0_f32; 3];
        let n = find_cubic_inflections(&cubic, &mut roots);
        let mut last_t = 0.0;
        for next_t in roots[..n].iter().copied().chain(std::iter::once(1.0)) {
            let mut quad_points = QuadConstruct::new();
            self.init_quad(StrokeType::Outer, last_t, next_t, &mut quad_points);
            self.cubic_stroke(&cubic, &mut quad_points);
            self.init_quad(StrokeType::Inner, last_t, next_t, &mut quad_points);
            self.cubic_stroke(&cubic, &mut quad_points);
            last_t = next_t;
        }

        if let Some(cusp) = find_cubic_cusp(&cubic) {
            let at = eval_cubic_pos_at(&cubic, cusp);
            push_circle(&mut self.cusper, at, self.radius);
        }

        // Emit the join even if one stroke succeeded but the last one failed: this avoids reversing an inner stroke with
        // a partial path followed by another move.
        let (normal_cd, unit_cd) = self.cubic_end_normal(&cubic, normal_ab, unit_ab);
        self.post_join_to(pt3, normal_cd, unit_cd);
    }

    fn cubic_end_normal(&self, cubic: &[Vec2; 4], normal_ab: Vec2, unit_ab: Vec2) -> (Vec2, Vec2) {
        let mut ab = cubic[1] - cubic[0];
        let mut cd = cubic[3] - cubic[2];
        let mut degenerate_ab = degenerate_vector(ab);
        let mut degenerate_cd = degenerate_vector(cd);
        if degenerate_ab && degenerate_cd {
            return (normal_ab, unit_ab);
        }
        if degenerate_ab {
            ab = cubic[2] - cubic[0];
            degenerate_ab = degenerate_vector(ab);
        }
        if degenerate_cd {
            cd = cubic[3] - cubic[1];
            degenerate_cd = degenerate_vector(cd);
        }
        if degenerate_ab || degenerate_cd {
            return (normal_ab, unit_ab);
        }
        set_normal_unit_normal2(cd, self.radius).unwrap_or((normal_ab, unit_ab))
    }

    fn cubic_stroke(&mut self, cubic: &[Vec2; 4], quad_points: &mut QuadConstruct) -> bool {
        if !self.found_tangents {
            let result = self.tangents_meet(cubic, quad_points);
            if result == ResultType::Quad {
                self.found_tangents = true;
            } else {
                let ok = points_within_dist(quad_points.quad[0], quad_points.quad[2], self.inv_res_scale);
                if (result == ResultType::Degenerate || ok) && self.cubic_mid_on_line(cubic, quad_points) {
                    self.add_degenerate_line(quad_points);
                    return true;
                }
            }
        }

        if self.found_tangents {
            let result = self.compare_quad_cubic(cubic, quad_points);
            if result == ResultType::Quad {
                let stroke = quad_points.quad;
                self.builder().quad_to(stroke[1], stroke[2]);
                return true;
            }
            if result == ResultType::Degenerate && !quad_points.opposite_tangents {
                self.add_degenerate_line(quad_points);
                return true;
            }
        }

        if !quad_points.quad[2].x.is_finite() || !quad_points.quad[2].y.is_finite() {
            return false; // projected quad isn't representable
        }
        if self.spend() {
            return false;
        }

        self.recursion_depth += 1;
        if self.recursion_depth > RECURSIVE_LIMITS[usize::from(self.found_tangents)] {
            return false;
        }

        let mut half = QuadConstruct::new();
        if !half.init_with_start(quad_points) {
            self.add_degenerate_line(quad_points);
            self.recursion_depth -= 1;
            return true;
        }
        if !self.cubic_stroke(cubic, &mut half) {
            return false;
        }
        if !half.init_with_end(quad_points) {
            self.add_degenerate_line(quad_points);
            self.recursion_depth -= 1;
            return true;
        }
        if !self.cubic_stroke(cubic, &mut half) {
            return false;
        }
        self.recursion_depth -= 1;
        true
    }

    /// The builder the current stroke type writes to.
    fn builder(&mut self) -> &mut Outline {
        if self.stroke_type == StrokeType::Outer { &mut self.outer } else { &mut self.inner }
    }

    fn cubic_mid_on_line(&self, cubic: &[Vec2; 4], quad_points: &mut QuadConstruct) -> bool {
        let mut mid = Vec2::ZERO;
        let mut cubic_mid_pt = Vec2::ZERO;
        self.cubic_perp_ray(cubic, quad_points.mid_t, &mut cubic_mid_pt, &mut mid, None);
        let dist = pt_to_line(mid, quad_points.quad[0], quad_points.quad[2]);
        dist < self.inv_res_scale_squared
    }

    /// Given a cubic and t: the point on the curve, its perpendicular, and the perpendicular tangent.
    fn cubic_perp_ray(&self, cubic: &[Vec2; 4], t: f32, t_pt: &mut Vec2, on_pt: &mut Vec2, tangent: Option<&mut Vec2>) {
        *t_pt = eval_cubic_pos_at(cubic, t);
        let mut dxy = eval_cubic_tangent_at(cubic, t);
        if dxy.x == 0.0 && dxy.y == 0.0 {
            let mut chopped_storage = [Vec2::ZERO; 7];
            let mut use_chopped = false;
            if is_nearly_zero(t) {
                dxy = cubic[2] - cubic[0];
            } else if is_nearly_zero(1.0 - t) {
                dxy = cubic[3] - cubic[1];
            } else {
                // If the cubic inflection falls on the cusp, subdivide the cubic to find the tangent there.
                let halves = super::geom::chop_cubic_at(*cubic, &[t]);
                chopped_storage.copy_from_slice(&halves[..7]);
                dxy = chopped_storage[3] - chopped_storage[2];
                if dxy.x == 0.0 && dxy.y == 0.0 {
                    dxy = chopped_storage[3] - chopped_storage[1];
                    use_chopped = true;
                }
            }
            if dxy.x == 0.0 && dxy.y == 0.0 {
                dxy = if use_chopped { chopped_storage[3] - chopped_storage[0] } else { cubic[3] - cubic[0] };
            }
        }
        self.set_ray_points(*t_pt, &mut dxy, on_pt, tangent);
    }

    fn compare_quad_cubic(&self, cubic: &[Vec2; 4], quad_points: &mut QuadConstruct) -> ResultType {
        // The quadratic approximation of the stroke.
        self.cubic_quad_ends(cubic, quad_points);
        let result = self.intersect_ray(IntersectRay::CtrlPt, quad_points);
        if result != ResultType::Quad {
            return result;
        }
        // Project a ray from the curve to the stroke: points near the midpoint of the quad and of the cubic.
        let (mut ray0, mut ray1) = (Vec2::ZERO, Vec2::ZERO);
        self.cubic_perp_ray(cubic, quad_points.mid_t, &mut ray1, &mut ray0, None);
        let quad = quad_points.quad;
        self.stroke_close_enough(&quad, &[ray0, ray1], quad_points)
    }

    /// Finds the start and end of the stroke quad for a t range, if they are not found yet.
    fn cubic_quad_ends(&self, cubic: &[Vec2; 4], quad_points: &mut QuadConstruct) {
        if !quad_points.start_set {
            let mut start_pt = Vec2::ZERO;
            let (mut on, mut tangent) = (Vec2::ZERO, Vec2::ZERO);
            self.cubic_perp_ray(cubic, quad_points.start_t, &mut start_pt, &mut on, Some(&mut tangent));
            quad_points.quad[0] = on;
            quad_points.tangent_start = tangent;
            quad_points.start_set = true;
        }
        if !quad_points.end_set {
            let mut end_pt = Vec2::ZERO;
            let (mut on, mut tangent) = (Vec2::ZERO, Vec2::ZERO);
            self.cubic_perp_ray(cubic, quad_points.end_t, &mut end_pt, &mut on, Some(&mut tangent));
            quad_points.quad[2] = on;
            quad_points.tangent_end = tangent;
            quad_points.end_set = true;
        }
    }

    fn finish_contour(&mut self, close: bool, curr_is_line: bool) {
        if self.segment_count > 0 {
            if close {
                let (before, pivot, after) = (self.prev_unit_normal, self.prev_pt, self.first_unit_normal);
                self.join(before, pivot, after, self.prev_is_line, curr_is_line);
                self.outer.close();
                // The inner path is its own contour, reversed.
                let pt = self.inner.last_point().unwrap_or_default();
                self.outer.move_to(pt);
                self.outer.reverse_path_to(&self.inner);
                self.outer.close();
            } else {
                // Cap the end, then walk the inner path back, then cap the start.
                let pt = self.inner.last_point().unwrap_or_default();
                cap(&mut self.outer, self.round_caps, self.prev_pt, self.prev_normal, pt);
                self.outer.reverse_path_to(&self.inner);
                cap(&mut self.outer, self.round_caps, self.first_pt, -self.first_normal, self.first_outer_pt);
                self.outer.close();
            }
            if !self.cusper.is_empty() {
                self.outer.append(&self.cusper);
                self.cusper.clear();
            }
        }
        // `inner` is reused: rewinding keeps its storage.
        self.inner.clear();
        self.segment_count = -1;
        self.first_outer_pt_index_in_contour = self.outer.point_count();
    }

    fn pre_join_to(&mut self, p: Vec2, curr_is_line: bool) -> Option<(Vec2, Vec2)> {
        debug_assert!(self.segment_count >= 0);
        let prev = self.prev_pt;
        let (normal, unit_normal) = if let Some(n) = set_normal_unit_normal(prev, p, self.res_scale, self.radius) {
            n
        } else if self.round_caps {
            // Round caps draw even for a zero-length segment. It has no direction, so it is drawn upright.
            (Vec2::new(self.radius, 0.0), Vec2::new(1.0, 0.0))
        } else {
            return None;
        };

        if self.segment_count == 0 {
            self.first_normal = normal;
            self.first_unit_normal = unit_normal;
            self.first_outer_pt = prev + normal;
            self.outer.move_to(self.first_outer_pt);
            self.inner.move_to(prev - normal);
        } else {
            // There is a previous segment.
            self.join(self.prev_unit_normal, prev, unit_normal, self.prev_is_line, curr_is_line);
        }
        self.prev_is_line = curr_is_line;
        Some((normal, unit_normal))
    }

    fn post_join_to(&mut self, p: Vec2, normal: Vec2, unit_normal: Vec2) {
        self.join_completed = true;
        self.prev_pt = p;
        self.prev_unit_normal = unit_normal;
        self.prev_normal = normal;
        self.segment_count += 1;
    }

    fn init_quad(&mut self, stroke_type: StrokeType, start: f32, end: f32, quad_points: &mut QuadConstruct) {
        self.stroke_type = stroke_type;
        self.found_tangents = false;
        quad_points.init(start, end);
    }

    fn quad_stroke(&mut self, quad: &[Vec2; 3], quad_points: &mut QuadConstruct) -> bool {
        let result = self.compare_quad_quad(quad, quad_points);
        if result == ResultType::Quad {
            let q = quad_points.quad;
            self.builder().quad_to(q[1], q[2]);
            return true;
        }
        if result == ResultType::Degenerate {
            self.add_degenerate_line(quad_points);
            return true;
        }

        self.recursion_depth += 1;
        if self.recursion_depth > RECURSIVE_LIMITS[QUAD_RECURSIVE_LIMIT] || self.spend() {
            return false;
        }
        let mut half = QuadConstruct::new();
        half.init_with_start(quad_points);
        if !self.quad_stroke(quad, &mut half) {
            return false;
        }
        half.init_with_end(quad_points);
        if !self.quad_stroke(quad, &mut half) {
            return false;
        }
        self.recursion_depth -= 1;
        true
    }

    fn compare_quad_quad(&mut self, quad: &[Vec2; 3], quad_points: &mut QuadConstruct) -> ResultType {
        // The quadratic approximation of the stroke.
        if !quad_points.start_set {
            let (mut start_pt, mut on, mut tangent) = (Vec2::ZERO, Vec2::ZERO, Vec2::ZERO);
            self.quad_perp_ray(quad, quad_points.start_t, &mut start_pt, &mut on, Some(&mut tangent));
            quad_points.quad[0] = on;
            quad_points.tangent_start = tangent;
            quad_points.start_set = true;
        }
        if !quad_points.end_set {
            let (mut end_pt, mut on, mut tangent) = (Vec2::ZERO, Vec2::ZERO, Vec2::ZERO);
            self.quad_perp_ray(quad, quad_points.end_t, &mut end_pt, &mut on, Some(&mut tangent));
            quad_points.quad[2] = on;
            quad_points.tangent_end = tangent;
            quad_points.end_set = true;
        }
        let result = self.intersect_ray(IntersectRay::CtrlPt, quad_points);
        if result != ResultType::Quad {
            return result;
        }
        // Project a ray from the curve to the stroke.
        let (mut ray0, mut ray1) = (Vec2::ZERO, Vec2::ZERO);
        self.quad_perp_ray(quad, quad_points.mid_t, &mut ray1, &mut ray0, None);
        let stroke = quad_points.quad;
        self.stroke_close_enough(&stroke, &[ray0, ray1], quad_points)
    }

    /// Given a point on the curve and its derivative, scales the derivative by the radius and finds the perpendicular
    /// point and its tangent.
    fn set_ray_points(&self, tp: Vec2, dxy: &mut Vec2, on_p: &mut Vec2, tangent: Option<&mut Vec2>) {
        *dxy = with_length(*dxy, self.radius).unwrap_or(Vec2::new(self.radius, 0.0));
        let axis_flip = f32::from(self.stroke_type as i8); // opposite ways for outer and inner
        *on_p = Vec2::new(tp.x + axis_flip * dxy.y, tp.y - axis_flip * dxy.x);
        if let Some(tangent) = tangent {
            *tangent = *on_p + *dxy;
        }
    }

    /// Given a quad and t: the point on the curve, its perpendicular, and the perpendicular tangent.
    fn quad_perp_ray(&self, quad: &[Vec2; 3], t: f32, tp: &mut Vec2, on_p: &mut Vec2, tangent: Option<&mut Vec2>) {
        *tp = eval_quad_at(quad, t);
        let mut dxy = eval_quad_tangent_at(quad, t);
        if dxy.x == 0.0 && dxy.y == 0.0 {
            dxy = quad[2] - quad[0];
        }
        self.set_ray_points(*tp, &mut dxy, on_p, tangent);
    }

    fn add_degenerate_line(&mut self, quad_points: &QuadConstruct) {
        let end = quad_points.quad[2];
        self.builder().line_to(end);
    }

    fn stroke_close_enough(&self, stroke: &[Vec2; 3], ray: &[Vec2; 2], quad_points: &QuadConstruct) -> ResultType {
        let stroke_mid = eval_quad_at(stroke, 0.5);
        // Measure the distance from the curve to the quad-stroke midpoint.
        if points_within_dist(ray[0], stroke_mid, self.inv_res_scale) {
            return if sharp_angle(&quad_points.quad) { ResultType::Split } else { ResultType::Quad };
        }
        // A quick reject on the distance to the quad's bounds.
        if !pt_in_quad_bounds(stroke, ray[0], self.inv_res_scale) {
            return ResultType::Split;
        }
        // Measure the curve ray distance to the quad-stroke.
        let mut roots = [0.0_f32; 3];
        let n = intersect_quad_ray(ray, stroke, &mut roots);
        if n != 1 {
            return ResultType::Split;
        }
        let quad_pt = eval_quad_at(stroke, roots[0]);
        let error = self.inv_res_scale * (1.0 - (roots[0] - 0.5).abs() * 2.0);
        if points_within_dist(ray[0], quad_pt, error) {
            return if sharp_angle(&quad_points.quad) { ResultType::Split } else { ResultType::Quad };
        }
        ResultType::Split
    }

    /// Finds the intersection of the stroke tangents to construct a stroke quad: whether the stroke is a degenerate
    /// (a line), a quad, or must be split, and optionally the quad's control point.
    fn intersect_ray(&self, kind: IntersectRay, quad_points: &mut QuadConstruct) -> ResultType {
        let start = quad_points.quad[0];
        let end = quad_points.quad[2];
        let a_len = quad_points.tangent_start - start;
        let b_len = quad_points.tangent_end - end;

        // The slopes match when this goes to zero.
        let denom = cross(a_len, b_len);
        if denom == 0.0 || !denom.is_finite() {
            quad_points.opposite_tangents = a_len.dot(b_len) < 0.0;
            return ResultType::Degenerate;
        }

        quad_points.opposite_tangents = false;
        let ab0 = start - end;
        let mut numer_a = cross(b_len, ab0);
        let numer_b = cross(a_len, ab0);
        if (numer_a >= 0.0) == (numer_b >= 0.0) {
            // The control point is outside the quad ends. If the perpendicular distances from the quad points to the
            // opposite tangent line are small, a straight line is good enough.
            let dist1 = pt_to_line(start, end, quad_points.tangent_end);
            let dist2 = pt_to_line(end, start, quad_points.tangent_start);
            if dist1.max(dist2) <= self.inv_res_scale_squared {
                return ResultType::Degenerate;
            }
            return ResultType::Split;
        }

        // Check whether the denominator is teeny relative to the numerator: if the offset by one is lost, the ratio is
        // too large.
        numer_a /= denom;
        let valid_divide = numer_a > numer_a - 1.0;
        if valid_divide {
            if kind == IntersectRay::CtrlPt {
                // The intersection of the tangents need not be on the tangent segment.
                quad_points.quad[1] = Vec2::new(
                    start.x * (1.0 - numer_a) + quad_points.tangent_start.x * numer_a,
                    start.y * (1.0 - numer_a) + quad_points.tangent_start.y * numer_a,
                );
            }
            return ResultType::Quad;
        }

        quad_points.opposite_tangents = a_len.dot(b_len) < 0.0;
        // If the lines are parallel, a straight line is good enough.
        ResultType::Degenerate
    }

    /// Given a cubic and a t range, whether the stroke can be described by a quad.
    fn tangents_meet(&self, cubic: &[Vec2; 4], quad_points: &mut QuadConstruct) -> ResultType {
        self.cubic_quad_ends(cubic, quad_points);
        self.intersect_ray(IntersectRay::ResultType, quad_points)
    }

    fn is_current_contour_empty(&self) -> bool {
        self.inner.is_zero_length_since_point(0)
            && self.outer.is_zero_length_since_point(self.first_outer_pt_index_in_contour)
    }

    // ----------------------------------------------------------------------------------------------------- joins

    /// Joins the previous segment (ending in `before_unit_normal`) to the next (starting in `after_unit_normal`).
    fn join(&mut self, before: Vec2, pivot: Vec2, after: Vec2, prev_is_line: bool, curr_is_line: bool) {
        let radius = self.radius;
        let inv_miter_limit = self.inv_miter_limit;
        let mut builders = Builders { inner: &mut self.inner, outer: &mut self.outer };
        match self.joiner {
            JoinKind::Miter => {
                miter_joiner(before, pivot, after, radius, inv_miter_limit, prev_is_line, curr_is_line, &mut builders);
            }
            JoinKind::Round => round_joiner(before, pivot, after, radius, &mut builders),
        }
    }
}
