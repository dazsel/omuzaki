//! Collision of a joint's tail sphere against a sphere or capsule collider (`VRMC_springBone` 1.0, "Collision with
//! colliders"), and against the extended colliders (`VRMC_springBone_extended_collider`): the inside of a sphere or
//! capsule, and a plane.
//!
//! A sphere is a capsule of zero length, so one code path serves both and the hot loop has no branch on which of them it is;
//! the inside shapes reuse the same nearest-axis-point maths and differ only in the sign of the distance, and a
//! plane is a dot product.

use omuzaki_core::glam::Vec3;

/// A collider placed in world space for this frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Collider {
    /// `1` to stay outside a sphere or capsule, `-1` to stay inside it. Folding the side into the sign of the
    /// distance keeps one code path (and the outside result bit-for-bit what it was) for all four shapes.
    side: f32,
    /// A plane: `start` is a point on it and `span` its unit normal.
    plane: bool,
    /// Centre of the start cap (the centre, for a sphere; a point on the plane, for a plane).
    start: Vec3,
    /// From the start cap to the end cap; zero for a sphere. A plane's unit normal.
    span: Vec3,
    /// `1 / |span|²`, or 0 when the span is too short to point anywhere (a sphere, or a degenerate capsule).
    inv_span_sq: f32,
    /// Radius of the caps and the cylinder (unused by a plane).
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
        Self { side: 1.0, plane: false, start: centre, span: Vec3::ZERO, inv_span_sq: 0.0, radius }
    }

    /// The volume within `radius` of the segment `start` to `end`.
    pub(super) fn capsule(start: Vec3, end: Vec3, radius: f32) -> Self {
        let span = end - start;
        let span_sq = span.length_squared();
        let inv_span_sq = if span_sq > MIN_SPAN_SQ { span_sq.recip() } else { 0.0 };
        Self { side: 1.0, plane: false, start, span, inv_span_sq, radius }
    }

    /// The volume within `radius` of `centre`, which a joint must stay inside.
    pub(super) fn sphere_inside(centre: Vec3, radius: f32) -> Self {
        Self { side: -1.0, ..Self::sphere(centre, radius) }
    }

    /// The volume within `radius` of the segment `start` to `end`, which a joint must stay inside.
    pub(super) fn capsule_inside(start: Vec3, end: Vec3, radius: f32) -> Self {
        Self { side: -1.0, ..Self::capsule(start, end, radius) }
    }

    /// A collider that touches nothing, standing in for a shape added to the contract after this solver.
    pub(super) fn inert() -> Self {
        // Everything is inside a ball this large, so nothing is ever pushed.
        Self::sphere_inside(Vec3::ZERO, f32::MAX)
    }

    /// The half-space on the side of `normal` (a unit vector in world space) from the plane through `point`.
    pub(super) fn plane(point: Vec3, normal: Vec3) -> Self {
        Self { side: 1.0, plane: true, start: point, span: normal, inv_span_sq: 0.0, radius: 0.0 }
    }

    /// How far to move a sphere of `joint_radius` at `point` to get it out of this collider (back into it, for an
    /// inside shape; onto the allowed side, for a plane), or `None` if it is already clear.
    ///
    /// This is the spec's `distance < 0` test and `nextTail - direction * distance` push in one: the offset is along
    /// the line from the collider's nearest axis point to `point`. A point on that axis (within
    /// [`DEGENERATE_DISTANCE`]) has no such line, so it is pushed along `fallback`, a unit vector the caller chooses
    /// deterministically. The spec's `normalized` of a zero vector would leave the point where it is; pushing along
    /// `fallback` is the one place this departs from the spec, and it is a choice the module docs record.
    pub(super) fn push_out(&self, point: Vec3, joint_radius: f32, fallback: Vec3) -> Option<Vec3> {
        let delta = point - self.start;
        if self.plane {
            let distance = delta.dot(self.span) - joint_radius;
            return (distance < 0.0).then(|| self.span * -distance);
        }
        // The spec's three cases (before the start cap, along the cylinder, past the end cap) are one clamp.
        let along = (self.span.dot(delta) * self.inv_span_sq).clamp(0.0, 1.0);
        let from_axis = delta - self.span * along;
        let length = from_axis.length();
        // Signed distance between the surfaces, negative when the joint is on the wrong side. For an inside shape it is
        // the room left to the radius, and the push toward the axis is `outward * distance` (a negative scalar).
        let distance = self.side * (length - self.radius) - joint_radius;
        if distance >= 0.0 {
            return None;
        }
        // A point on the axis has no direction to leave it in: use `fallback`. (Only a joint larger than an inside
        // shape can be both on its axis and past its radius.)
        let outward = if length >= DEGENERATE_DISTANCE { from_axis / length } else { fallback };
        Some(outward * (-self.side * distance))
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
    fn a_plane_pushes_a_joint_back_to_its_side_by_the_deficit() {
        let plane = Collider::plane(Vec3::ZERO, Vec3::Y);
        let push = plane.push_out(Vec3::new(0.0, -0.1, 0.0), 0.02, Vec3::X).unwrap();
        assert!(close(push, Vec3::new(0.0, 0.12, 0.0)), "{push}");
        assert!(close(Vec3::new(0.0, -0.1, 0.0) + push, Vec3::new(0.0, 0.02, 0.0)));
        assert_eq!(plane.push_out(Vec3::new(3.0, 0.02, 1.0), 0.02, Vec3::X), None, "touching is allowed");
        assert_eq!(plane.push_out(Vec3::new(0.0, 0.5, 0.0), 0.02, Vec3::X), None);
    }

    #[test]
    fn an_inside_sphere_pushes_a_joint_back_toward_the_centre() {
        let ball = Collider::sphere_inside(Vec3::ZERO, 0.5);
        let push = ball.push_out(Vec3::new(0.6, 0.0, 0.0), 0.0, Vec3::Z).unwrap();
        assert!(close(push, Vec3::new(-0.1, 0.0, 0.0)), "{push}");
        assert_eq!(ball.push_out(Vec3::new(0.4, 0.0, 0.0), 0.0, Vec3::Z), None);
        assert_eq!(ball.push_out(Vec3::new(0.5, 0.0, 0.0), 0.0, Vec3::Z), None, "touching stays");
        // The joint's own radius counts toward leaving.
        let push = ball.push_out(Vec3::new(0.4, 0.0, 0.0), 0.2, Vec3::Z).unwrap();
        assert!(close(push, Vec3::new(-0.1, 0.0, 0.0)), "{push}");
    }

    #[test]
    fn an_inside_capsule_measures_from_its_axis() {
        let capsule = Collider::capsule_inside(Vec3::ZERO, Vec3::Y, 0.1);
        let push = capsule.push_out(Vec3::new(0.2, 0.5, 0.0), 0.0, Vec3::Z).unwrap();
        assert!(close(push, Vec3::new(-0.1, 0.0, 0.0)), "{push}");
        // Past the end cap the cap is a ball about the end point.
        let push = capsule.push_out(Vec3::new(0.0, 1.3, 0.0), 0.0, Vec3::Z).unwrap();
        assert!(close(push, Vec3::new(0.0, -0.2, 0.0)), "{push}");
        assert_eq!(capsule.push_out(Vec3::new(0.05, 0.5, 0.0), 0.0, Vec3::Z), None);
    }

    #[test]
    fn a_joint_larger_than_an_inside_shape_on_its_axis_uses_the_fallback() {
        let ball = Collider::sphere_inside(Vec3::ZERO, 0.1);
        let push = ball.push_out(Vec3::ZERO, 0.3, Vec3::X).unwrap();
        assert!(close(push, Vec3::X * -0.2), "{push}");
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

    mod push_properties {
        use proptest::prelude::*;

        use super::*;

        fn vec3() -> impl Strategy<Value = Vec3> {
            prop::array::uniform3(-0.3_f32..0.3).prop_map(Vec3::from_array)
        }

        /// Distance from `p` to the segment `a`..`b`, written independently of the collider.
        fn to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
            let ab = b - a;
            let t =
                if ab.length_squared() > 0.0 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            (p - (a + ab * t)).length()
        }

        proptest! {
            /// The push itself (before any bone length is restored) leaves a joint on the allowed side, for planes whose
            /// normal is unit length and for inside shapes, and the run really does push: the first point is always
            /// placed in the forbidden region.
            #[test]
            fn the_push_leaves_every_joint_on_the_allowed_side(
                a in vec3(), b in vec3(), tilt in vec3(), radius in 0.05_f32..0.3, joint in 0.0_f32..0.05,
                points in prop::collection::vec(vec3(), 8), kind in 0_u8..3,
            ) {
                let normal = (Vec3::Y + tilt * 3.0).normalize();
                let (collider, room): (Collider, Box<dyn Fn(Vec3) -> f32>) = match kind {
                    0 => (Collider::plane(a, normal), Box::new(move |p| (p - a).dot(normal))),
                    1 => (Collider::sphere_inside(a, radius), Box::new(move |p| radius - (p - a).length())),
                    _ => (Collider::capsule_inside(a, b, radius), Box::new(move |p| radius - to_segment(p, a, b))),
                };
                // A point outside the inside shapes / behind the plane, so the push cannot be skipped vacuously.
                let beyond = match kind {
                    0 => a - normal * 0.2,
                    1 => a + Vec3::X * (radius + 0.2),
                    _ => b + Vec3::X * (radius + 0.2),
                };
                let mut pushed = 0;
                for point in std::iter::once(beyond).chain(points) {
                    if let Some(push) = collider.push_out(point, joint, Vec3::Y) {
                        pushed += 1;
                        let after = room(point + push) - joint;
                        prop_assert!(after >= -1e-5, "still {after} on the wrong side after the push");
                    }
                }
                prop_assert!(pushed >= 1, "nothing was pushed");
            }
        }
    }
}
