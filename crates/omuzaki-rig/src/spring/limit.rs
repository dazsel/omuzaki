//! `VRMC_springBone_limit` 1.0: the joint limits (cone, hinge, spherical) applied to a tentative tail direction.
//!
//! Source: <https://github.com/vrm-c/vrm-specification/tree/master/specification/VRMC_springBone_limit-1.0>, the
//! sections `Limits`, `Rotation`, `Limit Application Order` and the appendix pseudocode.
//!
//! # The frame a limit is evaluated in
//!
//! The appendix's `Rotation` listing builds the rotation that maps the limit's local space to the world:
//!
//! > `let rotation = joint.parent.worldRotation * joint.localSpaceInitialRotation * axisRotation * joint.limit.rotation;`
//!
//! so a limit is fixed to the head's *rest* frame under its parent's current world rotation. It does not turn with
//! the bone as it swings, which is what makes it a limit on the swing. `axisRotation` is the shortest rotation from
//! +Y to the bone's rest direction `boneAxis`, and for `boneAxis` exactly -Y (`dot == -1.0`) it is the half turn about
//! X (`Quaternion(1, 0, 0, 0)`). The spec's text says the same: "rotate each limit's local shape along the shortest
//! path that aligns the object's positive y-axis direction with the Head-to-Tail direction", then apply `rotation`
//! "in the local coordinate system defined by the default orientation".
//!
//! # Where it sits in a sub-step
//!
//! `Limit Application Order`: "When angular constraints by a limit are enabled, they **SHOULD** be applied at all of
//! the following times: Immediately after the inertia calculation; Immediately after each collision with a collider".
//! The appendix adds that "At the same points, the distance from the Joint position to `nextTail` is constrained to
//! preserve the spring length", so the limit replaces nothing: it follows the length constraint at both points, and
//! the length is preserved by it. Both points are in the solver's `integrate`.
//!
//! # Choices where the spec leaves room
//!
//! - Directions are tested against the limit with `<`, so a tail on the boundary stays put and a tail inside the limit
//!   is returned bit for bit (the spherical listing rebuilds `tailDir` from angles even when nothing is clamped; the
//!   result is the same direction, without the rounding).
//! - Singular directions use the spec's table exactly, tested with `==` as the listings do. The cone's
//!   `1 - y * y` is computed as `x * x + z * z`, the same for a unit vector without the cancellation near -Y, so the
//!   result is unit length to `f32` rounding.

use std::f32::consts::{FRAC_PI_2, PI};

use omuzaki_anim_api::spring::JointLimit;
use omuzaki_core::glam::{Quat, Vec3};

/// `axisRotation` of the spec's `Rotation` listing: the shortest rotation taking +Y to the unit `bone_axis`, or the
/// half turn about X when `bone_axis` is -Y.
pub(super) fn axis_rotation(bone_axis: Vec3) -> Quat {
    let dot = bone_axis.y;
    // `<=` rather than `==`: a unit vector can round to a hair below -1, which would give the zero quaternion below.
    if dot <= -1.0 {
        Quat::from_xyzw(1.0, 0.0, 0.0, 0.0)
    } else {
        Quat::from_xyzw(bone_axis.z, 0.0, -bone_axis.x, dot + 1.0).normalize()
    }
}

/// A joint's limit and the part of its frame that never changes.
#[derive(Copy, Clone, Debug)]
pub(super) struct Limiter {
    limit: JointLimit,
    /// `axisRotation * limit.rotation`: the limit's frame relative to the head's rest frame.
    local: Quat,
}

impl Limiter {
    /// The limiter for `limit` on a bone whose rest direction (in its rest frame) is the unit vector `bone_axis`.
    pub(super) fn new(limit: JointLimit, bone_axis: Vec3) -> Self {
        let rotation = match limit {
            JointLimit::Cone { rotation, .. }
            | JointLimit::Hinge { rotation, .. }
            | JointLimit::Spherical { rotation, .. } => rotation,
            // A limit this solver predates: unlimited, never a panic (the same stance as `ColliderShape`).
            _ => Quat::IDENTITY,
        };
        Self { limit, local: axis_rotation(bone_axis) * rotation.normalize() }
    }

    /// `dir` (a unit vector in the world) held inside the limit, given the head's rest frame in the world
    /// (`parent.worldRotation * localSpaceInitialRotation`).
    pub(super) fn apply(&self, rest_frame: Quat, dir: Vec3) -> Vec3 {
        apply(&self.limit, rest_frame * self.local, dir)
    }
}

/// `dir`, a unit vector in the world, held inside `limit` whose local space is turned by `frame` (the spec's
/// `rotation`: limit space to world). Returns a unit vector; `dir` itself, bit for bit, when it is inside.
pub(super) fn apply(limit: &JointLimit, frame: Quat, dir: Vec3) -> Vec3 {
    let local = frame.conjugate() * dir;
    let limited = match *limit {
        JointLimit::Cone { angle, .. } => cone(angle, local),
        JointLimit::Hinge { angle, .. } => hinge(angle, local),
        JointLimit::Spherical { pitch, yaw, .. } => spherical(pitch, yaw, local),
        _ => None,
    };
    limited.map_or(dir, |d| frame * d)
}

/// The cone listing. `None` when `dir` is inside.
///
/// The listing compares `y` with `cos(angle)`, which is blind to a tilt below about 5e-4 rad (`cos` of it rounds to 1),
/// so a zero-angle cone would let such a tail through. The tilt is measured as `atan2(horizontal, y)`, which keeps its
/// digits at every angle; the boundary it produces is the listing's.
fn cone(angle: f32, dir: Vec3) -> Option<Vec3> {
    let limit = angle.clamp(0.0, PI);
    // Computed as `x * x + z * z`, the listing's `1 - y * y` for a unit vector, without the cancellation near -Y.
    let horizontal = (dir.x * dir.x + dir.z * dir.z).sqrt();
    if horizontal.atan2(dir.y) <= limit {
        return None;
    }
    // `sin(pi)` is a hair below zero in `f32`; a negative scale would mirror the boundary.
    let (sin, cos) = (limit.sin().max(0.0), limit.cos());
    if horizontal == 0.0 {
        // Straight down: the positive Z side of the boundary.
        return Some(Vec3::new(0.0, cos, sin));
    }
    let scale = sin / horizontal;
    Some(Vec3::new(dir.x * scale, cos, dir.z * scale))
}

/// The hinge listing. `None` when `dir` is inside, which includes lying in the hinge's plane. The angle in the
/// hinge's plane is measured with `atan2` for the reason given at [`cone`].
fn hinge(angle: f32, dir: Vec3) -> Option<Vec3> {
    let limit = angle.clamp(0.0, PI);
    let projected = dir.y.hypot(dir.z);
    if projected == 0.0 {
        // Along X: straight up the hinge.
        return Some(Vec3::Y);
    }
    let (y, z) = (dir.y / projected, dir.z / projected);
    if dir.z.abs().atan2(dir.y) <= limit {
        // Inside the angle; only a component along X has to go.
        return (dir.x != 0.0).then(|| Vec3::new(0.0, y, z));
    }
    // Straight down (z == 0) takes the positive Z side.
    let z_sign = if z < 0.0 { -1.0 } else { 1.0 };
    Some(Vec3::new(0.0, limit.cos(), limit.sin().max(0.0) * z_sign))
}

/// The spherical listing. `None` when `dir` is inside. Yaw is `atan2(x, hypot(y, z))` rather than the listing's
/// `asin(x)`, which loses half its digits as `x` nears 1; for a unit vector they are the same angle.
fn spherical(pitch: f32, yaw: f32, dir: Vec3) -> Option<Vec3> {
    let (limit_pitch, limit_yaw) = (pitch.clamp(0.0, PI), yaw.clamp(0.0, FRAC_PI_2));
    let across = dir.y.hypot(dir.z);
    let mut pitch = if dir.y < 0.0 && dir.z == 0.0 {
        // Straight down in the plane (either sign of zero): the positive Z side, pitch pi.
        PI
    } else if across == 0.0 {
        // Along X: no pitch.
        0.0
    } else {
        dir.z.atan2(dir.y)
    };
    let mut yaw = dir.x.atan2(across);
    let mut limited = false;
    if pitch.abs() > limit_pitch {
        limited = true;
        pitch = limit_pitch * pitch.signum();
    }
    if yaw.abs() > limit_yaw {
        limited = true;
        yaw = limit_yaw * yaw.signum();
    }
    limited.then(|| Vec3::new(yaw.sin(), yaw.cos() * pitch.cos(), yaw.cos() * pitch.sin()))
}
