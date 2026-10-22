//! Guides generator (CB-32, #180): explicit clumps, hand-edited or frozen from another generator ([`edit`]).
//!
//! Each [`GuideClump`]'s control points are resolved against the head ([`GuidePoint::resolve`]: a `Surface` point on
//! the head's surface, so a head-shape change reshapes the hair; a `Head` point in the head's frame; a `Body` point in
//! bind space), and the clump's rest curve is the centripetal Catmull-Rom curve through them, resampled to equal arc
//! lengths: one point every [`SPACING`], at least two for each control point, between [`MIN_POINTS`] and
//! [`MAX_POINTS`]. Width, thickness and shade are the clump's own. The group draws no random numbers.
//!
//! [`edit`]: crate::hair::edit

use omuzaki_char_api::{GuideClump, GuidePoint, GuidesGen, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};

use crate::hair::compile::{CompiledGroup, HairGenCtx};
use crate::kit::catmull::spaced_points;

/// Arc length between neighbouring rest points, metres.
pub const SPACING: f32 = 0.006;
/// Fewest rest points a clump gets.
pub const MIN_POINTS: usize = 8;
/// Most rest points a clump gets.
pub const MAX_POINTS: usize = 96;
/// The length a clump whose points all coincide is drawn at, straight down, metres.
const DEGENERATE: f32 = 0.001;

/// Compiles the group: one clump per guide.
pub fn compile(g: &GuidesGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let clumps = g
        .clumps
        .iter()
        .map(|clump| ctx.clump(rest_curve(clump, ctx.head), clump.width, clump.thickness, clump.shade))
        .collect();
    CompiledGroup::tubes(clumps)
}

/// How many clumps [`compile`] makes: one per guide.
pub fn clump_count(g: &GuidesGen) -> usize {
    g.clumps.len()
}

/// The rest curve of `clump` over `head`, root first.
pub fn rest_curve(clump: &GuideClump, head: &dyn HeadSurface) -> Vec<Vec3> {
    let points: Vec<Vec3> = clump.points.iter().map(|p| p.resolve(head)).collect();
    through(&points)
}

/// The resampled Catmull-Rom curve through `points` (see the module docs). Fewer than two distinct points give a
/// [`DEGENERATE`] lock hanging from the first; no points give none.
pub fn through(points: &[Vec3]) -> Vec<Vec3> {
    let Some(&first) = points.first() else { return Vec::new() };
    let length: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    if !(length.is_finite() && length > 0.0) {
        return vec![first, first - Vec3::Y * DEGENERATE];
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a finite, positive length over a centimetre
    let by_length = (length / SPACING).ceil() as usize + 1;
    // Twice the control points at least, so a curve through dense points (a frozen style) keeps its tight bends.
    let n = by_length.max(2 * points.len() - 1).clamp(MIN_POINTS, MAX_POINTS);
    let control: Vec<DVec3> = points.iter().map(|p| p.as_dvec3()).collect();
    spaced_points(&control, n).map_or_else(|_| points.to_vec(), |curve| curve.iter().map(|p| p.as_vec3()).collect())
}

/// The point `p` (bind space) as a guide point over `head`: anchored to the surface ([`GuidePoint::Surface`]) when it
/// lies within `near` of it, so a head-shape change carries it; otherwise in the head's frame ([`GuidePoint::Head`]).
pub fn anchored(head: &dyn HeadSurface, p: Vec3, near: f32) -> GuidePoint {
    let from = p - head.centre();
    let distance = from.length();
    let dir = from / distance;
    if distance.is_finite() && distance > 0.0 {
        let offset = distance - head.radius(dir);
        if offset.is_finite() && (-0.05..=near.min(0.3)).contains(&offset) {
            let theta = f64::from(dir.y.clamp(-1.0, 1.0)).acos().to_degrees();
            let phi = f64::from(dir.x).atan2(f64::from(dir.z)).to_degrees();
            return GuidePoint::Surface([theta, phi, f64::from(offset)]);
        }
    }
    GuidePoint::Head(from.as_dvec3().to_array())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Ball;

    impl HeadSurface for Ball {
        fn centre(&self) -> Vec3 {
            Vec3::new(0.0, 1.5, 0.0)
        }

        fn radius(&self, _: Vec3) -> f32 {
            0.1
        }
    }

    #[test]
    fn a_point_near_the_head_anchors_to_it_and_resolves_back() {
        for p in [Vec3::new(0.0, 1.61, 0.0), Vec3::new(0.08, 1.45, -0.07), Vec3::new(0.0, 1.0, -0.12)] {
            let guide = anchored(&Ball, p, 0.03);
            assert!(guide.resolve(&Ball).distance(p) < 1e-5, "{p:?} as {guide:?}");
        }
        assert!(matches!(anchored(&Ball, Vec3::new(0.0, 1.61, 0.0), 0.03), GuidePoint::Surface(_)));
        assert!(matches!(anchored(&Ball, Vec3::new(0.0, 1.0, -0.12), 0.03), GuidePoint::Head(_)));
    }

    #[test]
    fn the_curve_passes_through_the_ends_at_even_spacing() {
        let points = [Vec3::new(0.0, 1.6, 0.0), Vec3::new(0.0, 1.5, -0.1), Vec3::new(0.0, 1.3, -0.12)];
        let curve = through(&points);
        assert_eq!(curve[0], points[0]);
        assert!(curve.last().unwrap().distance(points[2]) < 1e-5);
        let gaps: Vec<f32> = curve.windows(2).map(|w| w[0].distance(w[1])).collect();
        let (lo, hi) = gaps.iter().fold((f32::MAX, 0.0f32), |(lo, hi), g| (lo.min(*g), hi.max(*g)));
        assert!(hi - lo < 0.002, "{gaps:?}");
        assert_eq!(through(&[points[0], points[0]]).len(), 2, "a degenerate guide still draws");
    }
}
