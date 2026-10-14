//! Collision of a joint's tail sphere against a sphere or capsule collider (`VRMC_springBone` 1.0, "Collision with
//! colliders").
//!
//! A sphere is a capsule of zero length, so one code path serves both and the hot loop has no branch on the shape.

use omuzaki_core::glam::Vec3;

/// A collider placed in world space for this frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Collider {
    /// Centre of the start cap (the centre, for a sphere).
    start: Vec3,
    /// From the start cap to the end cap; zero for a sphere.
    span: Vec3,
    /// `1 / |span|²`, or 0 when the span is too short to point anywhere (a sphere, or a degenerate capsule).
    inv_span_sq: f32,
    /// Radius of the caps and the cylinder.
    radius: f32,
}

/// Capsules shorter than this (squared, metres) are spheres: dividing by their length would only amplify rounding.
const MIN_SPAN_SQ: f32 = 1e-12;

/// A point this close (metres) to a collider's axis counts as on it, so rounding residue cannot pick the push direction.
/// About three ulps of `f32` at 32 m from the skeleton root, and far below anything visible.
pub(super) const DEGENERATE_DISTANCE: f32 = 1e-5;

impl Collider {
    /// A ball.
    pub(super) fn sphere(centre: Vec3, radius: f32) -> Self {
        Self { start: centre, span: Vec3::ZERO, inv_span_sq: 0.0, radius }
    }

    /// The volume within `radius` of the segment `start` to `end`.
    pub(super) fn capsule(start: Vec3, end: Vec3, radius: f32) -> Self {
        let span = end - start;
        let span_sq = span.length_squared();
        let inv_span_sq = if span_sq > MIN_SPAN_SQ { span_sq.recip() } else { 0.0 };
        Self { start, span, inv_span_sq, radius }
    }

    /// How far to move a sphere of `joint_radius` at `point` to get it out of this collider, or `None` if it is
    /// already clear.
    ///
    /// This is the spec's `distance < 0` test and `nextTail - direction * distance` push in one: the offset is along
    /// the line from the collider's nearest axis point to `point`. A point on that axis (within
    /// [`DEGENERATE_DISTANCE`]) has no such line, so it is pushed along `fallback`, a unit vector the caller chooses
    /// deterministically. The spec's `normalized` of a zero vector would leave the point where it is; pushing along
    /// `fallback` is the one place this departs from the spec, and it is a choice the module docs record.
    pub(super) fn push_out(&self, point: Vec3, joint_radius: f32, fallback: Vec3) -> Option<Vec3> {
        let delta = point - self.start;
        // The spec's three cases (before the start cap, along the cylinder, past the end cap) are one clamp.
        let along = (self.span.dot(delta) * self.inv_span_sq).clamp(0.0, 1.0);
        let from_axis = delta - self.span * along;
        let length = from_axis.length();
        let distance = length - self.radius - joint_radius;
        if distance >= 0.0 {
            return None;
        }
        let outward = if length >= DEGENERATE_DISTANCE { from_axis / length } else { fallback };
        Some(outward * -distance)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // the expected values are exactly representable
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-6
    }

    #[test]
    fn a_sphere_pushes_a_point_out_along_the_line_from_its_centre() {
        let ball = Collider::sphere(Vec3::new(1.0, 0.0, 0.0), 0.5);
        // 0.2 from the centre, so 0.3 inside the surface (joint radius 0): pushed 0.3 further out along +y.
        let push = ball.push_out(Vec3::new(1.0, 0.2, 0.0), 0.0, Vec3::Z).unwrap();
        assert!(close(push, Vec3::new(0.0, 0.3, 0.0)), "{push}");
        // A joint with a radius is stopped that much earlier.
        let push = ball.push_out(Vec3::new(1.0, 0.2, 0.0), 0.1, Vec3::Z).unwrap();
        assert!(close(push, Vec3::new(0.0, 0.4, 0.0)), "{push}");
    }

    #[test]
    fn a_clear_point_and_a_touching_point_are_not_pushed() {
        let ball = Collider::sphere(Vec3::ZERO, 0.5);
        assert_eq!(ball.push_out(Vec3::new(0.6, 0.0, 0.0), 0.0, Vec3::Y), None);
        assert_eq!(ball.push_out(Vec3::new(0.5, 0.0, 0.0), 0.0, Vec3::Y), None, "distance 0 is touching, not inside");
        assert_eq!(ball.push_out(Vec3::new(0.7, 0.0, 0.0), 0.1, Vec3::Y), None);
    }

    #[test]
    fn a_tail_on_the_centre_is_pushed_along_the_fallback_direction() {
        let ball = Collider::sphere(Vec3::ONE, 0.5);
        let up = Vec3::Y;
        let push = ball.push_out(Vec3::ONE, 0.1, up).unwrap();
        // The tail lands radius + joint radius (0.6) from the centre, along the fallback.
        let landed = Vec3::ONE + push;
        assert!(close(landed - Vec3::ONE, up * 0.6), "{landed}");
        // Within the degenerate distance counts as on the centre.
        let nearly = ball.push_out(Vec3::ONE + Vec3::X * 1e-7, 0.1, up).unwrap();
        assert!(close(nearly, up * 0.6), "{nearly}");
        // Deterministic: the same inputs give the same bits.
        let again = ball.push_out(Vec3::ONE, 0.1, up).unwrap();
        assert_eq!(push.to_array().map(f32::to_bits), again.to_array().map(f32::to_bits));
    }

    #[test]
    fn a_capsule_has_three_regions() {
        let capsule = Collider::capsule(Vec3::ZERO, Vec3::new(0.0, 2.0, 0.0), 0.5);
        // Beside the cylinder: pushed sideways only.
        let side = capsule.push_out(Vec3::new(0.2, 1.0, 0.0), 0.0, Vec3::Z).unwrap();
        assert!(close(side, Vec3::new(0.3, 0.0, 0.0)), "{side}");
        // Past the end cap: the cap is a sphere about the end point.
        let above = capsule.push_out(Vec3::new(0.0, 2.3, 0.0), 0.0, Vec3::Z).unwrap();
        assert!(close(above, Vec3::new(0.0, 0.2, 0.0)), "{above}");
        // Before the start cap: likewise about the start point.
        let below = capsule.push_out(Vec3::new(0.0, -0.3, 0.0), 0.0, Vec3::Z).unwrap();
        assert!(close(below, Vec3::new(0.0, -0.2, 0.0)), "{below}");
        // Clear of every region.
        assert_eq!(capsule.push_out(Vec3::new(0.0, 2.6, 0.0), 0.0, Vec3::Z), None);
        assert_eq!(capsule.push_out(Vec3::new(0.6, 1.0, 0.0), 0.0, Vec3::Z), None);
    }

    #[test]
    fn a_degenerate_capsule_is_a_sphere() {
        let point = Vec3::new(0.0, 0.1, 0.0);
        let flat = Collider::capsule(Vec3::ZERO, Vec3::ZERO, 0.5);
        let ball = Collider::sphere(Vec3::ZERO, 0.5);
        assert_eq!(flat.push_out(point, 0.0, Vec3::Z), ball.push_out(point, 0.0, Vec3::Z));
        let nearly = Collider::capsule(Vec3::ZERO, Vec3::new(1e-7, 0.0, 0.0), 0.5);
        let push = nearly.push_out(point, 0.0, Vec3::Z).unwrap();
        assert!(push.is_finite() && close(push, Vec3::new(0.0, 0.4, 0.0)), "{push}");
    }

    #[test]
    fn a_point_on_the_axis_of_a_capsule_is_pushed_along_the_fallback_direction() {
        let capsule = Collider::capsule(Vec3::ZERO, Vec3::new(0.0, 2.0, 0.0), 0.5);
        let push = capsule.push_out(Vec3::new(0.0, 1.0, 0.0), 0.0, Vec3::X).unwrap();
        assert!(close(push, Vec3::X * 0.5), "{push}");
    }
}
