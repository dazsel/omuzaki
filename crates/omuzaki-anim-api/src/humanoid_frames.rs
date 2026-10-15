//! The normalised-pose conversion (spec §8, §20, RG-01): VRM humanoid rotations to and from one skeleton's own bones.
//!
//! # What "normalised" means, and the one formula
//!
//! Animation speaks [`HumanoidPose`], in the *normalised* space of VRM Animation: every bone's rest orientation is the
//! identity, so a rotation means "turn this bone this much, about the world's axes as they are in the T-pose".
//! The same numbers move any character's arm the same way. A real skeleton does not have identity rest rotations: bones
//! exported from Blender or a VRM 0.x model carry arbitrary local axes (often with the bone's Y along the limb). So the
//! solve converts.
//!
//! Let `Wp` be the skeleton-space rest rotation of a bone's parent, `Wb` the bone's own, and `q` its normalised
//! rotation. In normalised space the bone's world rotation is `D·q`, where `D` is the parent's accumulated rotation.
//! On the real skeleton that same motion is `D·Wb` for the bone and `D·Wp` for its parent, so the bone's new
//! parent-local rotation is `(D·Wp)⁻¹ · D·q·Wb`, in which `D` cancels:
//!
//! ```text
//! local = Wp⁻¹ · q · Wb
//! ```
//!
//! Two multiplies per bone, with `Wp` and `Wb` read from a table built once. This is the conversion three-vrm's
//! normalised rig performs. The inverse is `q = Wp · local · Wb⁻¹`. Both directions live here so the table has one
//! owner: `omuzaki-rig` poses a character with it, and an importer reads an authored A-pose back as a [`HumanoidPose`].
//!
//! Optional humanoid bones a skeleton lacks are skipped: a rotation for an absent bone is ignored (as VRM Animation
//! specifies) and the children hang from the nearest present ancestor, which is what `Wp` of the child already is.
//! A *generic* skeleton ([`Skeleton::generic`]) lacks every humanoid bone, so there is nothing to turn.
//!
//! The hips offset is in the same world-aligned rest axes, so it is carried into the hips parent's frame (`Wp⁻¹`)
//! before it is added to the hips translation.

use omuzaki_core::glam::{Quat, Vec4};

use crate::skeleton::{HumanoidBone, HumanoidPose, Pose, Skeleton, Transform};

/// The `rest_local` slice a conversion was given does not have one transform per joint of the skeleton the table was
/// built from. Nothing is written when this is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("rest_local has {actual} transforms, but the skeleton has {expected} joints")]
pub struct RestLengthMismatch {
    /// The skeleton's joint count, as [`HumanoidFrames::new`] recorded it.
    pub expected: usize,
    /// The length that was given.
    pub actual: usize,
}

/// `q` scaled to unit length, or `None` if it is zero or not finite (so a bad input can be skipped, not propagated).
fn unit(q: Quat) -> Option<Quat> {
    Vec4::from(q).try_normalize().map(Quat::from_vec4)
}

/// One present humanoid bone, with the two rest rotations the conversion needs.
#[derive(Copy, Clone, Debug)]
struct Binding {
    /// Index into [`HumanoidPose::rotations`].
    bone: usize,
    /// Index into [`Pose::local`].
    joint: usize,
    /// `Wp`: skeleton-space rest rotation of the parent (identity for a root).
    parent_world: Quat,
    /// `Wb`: skeleton-space rest rotation of the bone itself.
    own_world: Quat,
}

/// The two rest rotations the conversion needs for each humanoid bone a skeleton has: `Wp`, the skeleton-space rest
/// rotation of the parent (identity for a root), and `Wb`, the bone's own. Cold: built once per skeleton.
///
/// Empty for a generic skeleton, where both conversions leave every bone at rest.
#[derive(Clone, Debug)]
pub struct HumanoidFrames {
    /// Every humanoid bone the skeleton has, in [`HumanoidBone::ALL`] order.
    bindings: Vec<Binding>,
    /// The hips: the only bone whose position the pose moves. `None` for a generic skeleton.
    hips: Option<Binding>,
    /// The skeleton's joint count. A skeleton has at most [`MAX_BONES`](crate::skeleton::MAX_BONES) of them, so a
    /// `rest_local` of this length always fits a [`Pose`].
    joints: usize,
}

impl HumanoidFrames {
    /// Builds the table for `skeleton` from its rest pose's forward kinematics. Cold: allocates. Empty for a generic
    /// skeleton.
    pub fn new(skeleton: &Skeleton) -> Self {
        let mut world = Pose::IDENTITY;
        skeleton.world_pose(&skeleton.rest_pose(), &mut world);
        let rest_rotation = |id: usize| world.local.get(id).map(|t| t.rotation.normalize());
        let bindings: Vec<Binding> = HumanoidBone::ALL
            .into_iter()
            .filter_map(|bone| {
                let id = skeleton.humanoid_bone(bone)?;
                let parent_world = match skeleton.get(id)?.parent {
                    Some(parent) => rest_rotation(parent.index())?,
                    None => Quat::IDENTITY,
                };
                Some(Binding {
                    bone: bone.index(),
                    joint: id.index(),
                    parent_world,
                    own_world: rest_rotation(id.index())?,
                })
            })
            .collect();
        let hips = bindings.iter().copied().find(|b| b.bone == HumanoidBone::Hips.index());
        Self { bindings, hips, joints: skeleton.len() }
    }

    /// How many humanoid bones the skeleton has (0 for a generic skeleton).
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Whether the skeleton has no humanoid bones (it is generic).
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Checks `rest_local` against the joint count: the one comparison both conversions start with.
    fn check(&self, rest_local: &[Transform]) -> Result<(), RestLengthMismatch> {
        if rest_local.len() == self.joints {
            Ok(())
        } else {
            Err(RestLengthMismatch { expected: self.joints, actual: rest_local.len() })
        }
    }

    /// `q = Wp · local · Wb⁻¹` per present bone; a bone the skeleton lacks, or a zero or non-finite result, reads as the
    /// identity; `hips_offset = Wp · (local hips translation − rest hips translation)`. `rest_local` is the skeleton's
    /// rest transforms, indexed by joint, and must have one entry per joint.
    ///
    /// # Errors
    /// [`RestLengthMismatch`] if `rest_local` is not as long as the skeleton has joints. `out` is left untouched.
    /// Hot path: no allocation.
    #[inline]
    pub fn to_humanoid(
        &self,
        rest_local: &[Transform],
        local: &Pose,
        out: &mut HumanoidPose,
    ) -> Result<(), RestLengthMismatch> {
        self.check(rest_local)?;
        *out = HumanoidPose::REST;
        for b in &self.bindings {
            let q = b.parent_world * local.local[b.joint].rotation * b.own_world.conjugate();
            out.rotations[b.bone] = unit(q).unwrap_or(Quat::IDENTITY);
        }
        if let Some(hips) = self.hips {
            let moved = local.local[hips.joint].translation - rest_local[hips.joint].translation;
            out.hips_offset = hips.parent_world * moved;
        }
        Ok(())
    }

    /// `local = Wp⁻¹ · q · Wb` per present bone, on top of a copy of `rest_local`: humanoid bones turned by the pose,
    /// everything else at rest. `rest_local` must have one entry per joint; since a skeleton has at most
    /// [`MAX_BONES`](crate::skeleton::MAX_BONES) joints, that also keeps the copy inside [`Pose`]. A rotation that is not
    /// finite (or is zero) leaves that bone at rest, and a non-finite hips offset is ignored, so a bad input never
    /// reaches the renderer as a NaN matrix.
    ///
    /// # Errors
    /// [`RestLengthMismatch`] if `rest_local` is not as long as the skeleton has joints. `out` is left untouched.
    /// Hot path: no allocation.
    #[inline]
    pub fn apply_humanoid(
        &self,
        rest_local: &[Transform],
        pose: &HumanoidPose,
        out: &mut Pose,
    ) -> Result<(), RestLengthMismatch> {
        self.check(rest_local)?;
        out.local[..rest_local.len()].copy_from_slice(rest_local);
        for b in &self.bindings {
            let local = b.parent_world.conjugate() * pose.rotations[b.bone] * b.own_world;
            if let Some(local) = unit(local) {
                out.local[b.joint].rotation = local;
            }
        }
        if let Some(hips) = self.hips
            && pose.hips_offset.is_finite()
        {
            out.local[hips.joint].translation += hips.parent_world.conjugate() * pose.hips_offset;
        }
        Ok(())
    }
}
