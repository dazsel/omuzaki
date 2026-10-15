//! VRM node constraints as a solver stage (spec v3 §27, EX-11; contract: [`omuzaki_anim_api::constraint`]).
//!
//! [`VrmConstraintSolver`] follows `VRMC_node_constraint-1.0`: roll, aim and rotation constraints, each writing the
//! local rotation of its destination bone. The reference pseudocode is non-normative; the choices made where it is
//! silent are listed here:
//! - a constraint whose weight is 0 leaves the destination at its rest rotation (the spec slerps *from rest*), so a
//!   constrained bone ignores whatever rotation it had before the stage ran;
//! - an aim whose source sits on the destination (zero distance) leaves the destination at rest;
//! - an antiparallel aim turns half a revolution about an axis perpendicular to the aim axis, fixed by the vectors
//!   alone (glam's `from_rotation_arc`), so the result is deterministic;
//! - only rotations are written; translations are never touched.
//!
//! ## Where it goes in a frame
//!
//! After the humanoid pose, look-at and IK have written the pose, and before spring bones (the order of
//! `VRMC_vrm-1.0`): constraints read their sources' rotations *as posed*, and springs then move on top of the
//! constrained bones. In the composed rig (`full::FullRig`) that is between the IK stage and the spring stage, set with
//! [`FullRig::with_constraints`](crate::full::FullRig::with_constraints).

use omuzaki_anim_api::{
    Axis, BoneConstraint, ConstraintError, ConstraintKind, ConstraintSet, Pose, SignedAxis, Skeleton, Transform,
};
use omuzaki_core::glam::{Quat, Vec3};

use crate::ik::look_at::swing;

/// Runs a [`ConstraintSet`] over a pose, in dependency order.
#[derive(Clone, Debug)]
pub struct VrmConstraintSolver {
    /// The constraints, already in the order they run.
    ordered: Vec<BoneConstraint>,
}

impl VrmConstraintSolver {
    /// Validates `set` against `skeleton` and fixes the run order. Everything `apply` needs is allocated here.
    ///
    /// # Errors
    /// Whatever [`ConstraintSet::validate`] reports.
    pub fn new(set: &ConstraintSet, skeleton: &Skeleton) -> Result<Self, ConstraintError> {
        set.validate(skeleton)?;
        let ordered = set.order()?.into_iter().map(|i| set.constraints[i]).collect();
        Ok(Self { ordered })
    }

    /// Writes the constrained bones' local rotations into `pose`. Hot path: no allocation, no locking.
    ///
    /// `pose` holds local transforms for `skeleton`, as left by the stages before this one.
    pub fn apply(&self, skeleton: &Skeleton, pose: &mut Pose) {
        for c in &self.ordered {
            let rotation = solve_one(c, skeleton, pose);
            pose.local[c.bone.index()].rotation = rotation;
        }
    }
}

fn rest_of(skeleton: &Skeleton, index: usize) -> Transform {
    skeleton.bones().get(index).map_or(Transform::IDENTITY, |b| b.rest)
}

/// Transform of bone `index` relative to the skeleton root, walking up the parents (so no scratch pose is needed).
fn world_of(skeleton: &Skeleton, pose: &Pose, index: usize) -> Transform {
    let mut acc = pose.local[index];
    let mut parent = skeleton.bones().get(index).and_then(|b| b.parent);
    while let Some(p) = parent {
        acc = pose.local[p.index()] * acc;
        parent = skeleton.bones().get(p.index()).and_then(|b| b.parent);
    }
    acc
}

fn axis_vec(axis: Axis) -> Vec3 {
    match axis {
        Axis::X => Vec3::X,
        Axis::Y => Vec3::Y,
        Axis::Z => Vec3::Z,
    }
}

fn signed_axis_vec(axis: SignedAxis) -> Vec3 {
    match axis {
        SignedAxis::PositiveX => Vec3::X,
        SignedAxis::NegativeX => Vec3::NEG_X,
        SignedAxis::PositiveY => Vec3::Y,
        SignedAxis::NegativeY => Vec3::NEG_Y,
        SignedAxis::PositiveZ => Vec3::Z,
        SignedAxis::NegativeZ => Vec3::NEG_Z,
    }
}

/// The part of `q` that turns about the unit `axis` (swing-twist decomposition by projection); the identity when `q`
/// is a half turn across it, where the twist is undefined.
fn twist(q: Quat, axis: Vec3) -> Quat {
    let along = axis * Vec3::new(q.x, q.y, q.z).dot(axis);
    let t = Quat::from_xyzw(along.x, along.y, along.z, q.w);
    if t.length_squared() > f32::EPSILON { t.normalize() } else { Quat::IDENTITY }
}

fn solve_one(c: &BoneConstraint, skeleton: &Skeleton, pose: &Pose) -> Quat {
    let d = c.bone.index();
    let s = c.source.index();
    let d_rest = rest_of(skeleton, d).rotation;
    let target = match c.kind {
        ConstraintKind::Rotation => {
            // dS = qS_rest^-1 * qS, applied in the destination's local space.
            let delta = rest_of(skeleton, s).rotation.inverse() * pose.local[s].rotation;
            d_rest * delta
        }
        ConstraintKind::Roll { axis } => {
            let s_rest = rest_of(skeleton, s).rotation;
            // The source's change, taken to the destination's rest space: dD = qD_rest^-1 * (qS_rest dS qS_rest^-1) * qD_rest.
            let delta_parent = pose.local[s].rotation * s_rest.inverse();
            let delta = d_rest.inverse() * delta_parent * d_rest;
            let axis = axis_vec(axis);
            d_rest * twist(delta, axis)
        }
        ConstraintKind::Aim { axis } => {
            let parent_world = skeleton
                .bones()
                .get(d)
                .and_then(|b| b.parent)
                .map_or(Transform::IDENTITY, |p| world_of(skeleton, pose, p.index()));
            let to = world_of(skeleton, pose, s).translation - (parent_world * pose.local[d]).translation;
            match to.try_normalize() {
                Some(to) => {
                    let q_parent = parent_world.rotation;
                    let from = q_parent * d_rest * signed_axis_vec(axis);
                    let turn = swing(from, to);
                    q_parent.inverse() * turn * q_parent * d_rest
                }
                None => d_rest,
            }
        }
    };
    d_rest.slerp(target, c.weight).normalize()
}
