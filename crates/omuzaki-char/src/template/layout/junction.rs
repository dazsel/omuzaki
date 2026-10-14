//! Blending a leg into the pelvis at the hip, so the front outline is C1 across the crotch.
//!
//! The body is lofted region by region, and its outline is the outer edge of their union. Where a thigh leaves the
//! pelvis two independently drawn curves meet, and unless they happen to touch with the same direction the outline
//! steps (the hip's ledge). Tables cannot promise the touch: the body shape scales the torso and the legs by different
//! gains at the very same heights. (The shoulder is the body mesher's: `body/junction.rs` grows the arm a root.)
//!
//! So the leg is built to *follow the torso's outline* where it leaves it. For the thigh's outer edge a [`Join`]
//! replaces the shaped edge with a mix, from the torso's own outline (the thigh's edge lies exactly on it, so the
//! outline is the torso's there) to the thigh's own edge. The weight of the mix is a smoothstep, flat at both ends, so
//! the thigh leaves the torso's outline with the torso's direction: the union of the two is then C1 whatever the gains
//! were. Only the one edge that forms the outline moves; the inner, front and back edges are the thigh's own.

use omuzaki_core::glam::DVec2;

use super::{Gain, Region, frame};
use crate::kit::curve::{fraction, sstep};

/// Sections of the torso that its outline is drawn through.
const OUTLINE_STEPS: usize = 192;
/// Points the outline's extension below the torso is drawn through.
const EXTENSION_STEPS: usize = 16;
/// Over how much of the torso's length, as a fraction, the outline's slope fades after the torso's bottom.
const OUTLINE_FADE: f64 = 0.15;
/// How far past the torso's bottom the outline carries on along its last direction, as a fraction of the torso's length.
const OUTLINE_EXTENSION: f64 = 0.5;
/// Over how much of the torso's length, as a fraction, a thigh blends from the pelvis's outline to its own edge below
/// the torso's bottom: the thigh's own edge may be a long way from the pelvis's outline's continuation.
const HIP_BLEND_BELOW: f64 = 0.2;
/// The same above the torso's bottom, where the thigh runs up inside the pelvis and its own edge, tucked in, can be far
/// inside the outline: the longer the blend, the gentler the turn at the crotch.
const HIP_BLEND_ABOVE: f64 = 0.45;

/// The front outline of the left half of the torso, which the legs follow.
#[derive(Clone, Debug)]
pub(super) struct Outline {
    /// From the middle of the torso's top cap, down its edge, and on past its bottom.
    points: Vec<DVec2>,
    /// The height of the torso's bottom section.
    bottom: f64,
    /// The torso's length.
    length: f64,
}

impl Outline {
    /// The outline of `torso` (the left half; the torso is its own mirror image).
    pub(super) fn of(torso: &Region) -> Self {
        let edge = |t: f64| {
            let s = torso.section(t);
            DVec2::new(s.centre.x + s.radii.x, s.centre.y)
        };
        let top = torso.section(1.0).centre;
        let mut points = vec![DVec2::new(top.x, top.y)];
        points.extend((0..=OUTLINE_STEPS).rev().map(|k| edge(fraction(k, OUTLINE_STEPS))));
        let (bottom, above) = (points[points.len() - 1], points[points.len() - 2]);
        let length = top.y - bottom.y;
        // The pelvis's edge carried on below its bottom with the slope it ends with, which fades away over a short
        // distance: the thigh blends into a line that is not crossing the axis however long it is, and where the
        // outline's slope is carried on, it is C1 with the torso's.
        let slope = (bottom.x - above.x) / (above.y - bottom.y).max(f64::EPSILON);
        let fade = OUTLINE_FADE * length;
        points.extend((1..=EXTENSION_STEPS).map(|k| {
            let d = OUTLINE_EXTENSION * length * fraction(k, EXTENSION_STEPS);
            DVec2::new(bottom.x + slope * fade * (1.0 - (-d / fade).exp()), bottom.y - d)
        }));
        Self { points, bottom: bottom.y, length }
    }

    /// How far from `from` along the unit vector `dir` the outline lies, if it does: where the ray leaves the torso, the
    /// farthest crossing (the shoulder's slope can be crossed once before the side).
    fn reach(&self, from: DVec2, dir: DVec2) -> Option<f64> {
        self.points
            .windows(2)
            .filter_map(|seg| {
                let edge = seg[1] - seg[0];
                let denom = dir.perp_dot(edge);
                if denom.abs() < 1e-12 {
                    return None;
                }
                let to_start = seg[0] - from;
                let (r, u) = (to_start.perp_dot(edge) / denom, to_start.perp_dot(dir) / denom);
                (r > 0.0 && (0.0..=1.0).contains(&u)).then_some(r)
            })
            .max_by(f64::total_cmp)
    }
}

/// A leg's join to the torso.
#[derive(Copy, Clone, Debug)]
pub(super) struct Join<'a> {
    pub(super) outline: &'a Outline,
}

impl Join<'_> {
    /// Replaces the outer gain in `samples` (the leg's gains at arc lengths `s`) so the leg's outer edge follows the
    /// torso's outline where it leaves it. `shaped` is the outer gain of each sample before anything tucked the leg into
    /// the torso: below the torso it is the thigh's own edge.
    pub(super) fn apply(self, samples: &mut [(f64, Gain)], shaped: &[f64], base: &Region) {
        let length = base.length();
        for (k, (s, gain)) in samples.iter_mut().enumerate() {
            let section = base.section(*s / length);
            let (across, _) = frame(section.axis, base.front);
            let (from, dir, rx) =
                (DVec2::new(section.centre.x, section.centre.y), DVec2::new(across.x, across.y), section.radii.x);
            let w = self.weight(from.y);
            // Below the torso's bottom the pelvis no longer holds the thigh in: it is the shaped thigh, whether or not it
            // blends there (the tuck's scaling must not end in a step where the blend begins).
            if from.y < self.outline.bottom {
                gain.outer = shaped[k];
            }
            // The search along the outline is the cost of a blend: only where the blend needs it.
            let Some(host) = (w > 0.0).then(|| self.outline.reach(from, dir)).flatten() else { continue };
            if rx <= 0.0 {
                continue;
            }
            gain.outer = (w * host + (1.0 - w) * gain.outer * rx) / rx;
        }
    }

    /// The weight of the torso's outline for a section at height `y`: `1` at the torso's bottom, `0` a blend length above
    /// and below it, flat at all three.
    fn weight(self, y: f64) -> f64 {
        let (above, below) = (HIP_BLEND_ABOVE * self.outline.length, HIP_BLEND_BELOW * self.outline.length);
        let depth = self.outline.bottom - y;
        if depth < 0.0 { sstep(-above, 0.0, depth) } else { 1.0 - sstep(0.0, below, depth) }
    }
}
