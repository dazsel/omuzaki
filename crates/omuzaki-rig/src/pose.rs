//! The humanoid pose solve: VRM normalised rotations onto one skeleton's own bones (spec §8, §20, RG-01).
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
//! Two multiplies per bone, with `Wp` and `Wb` read from tables built once. This is the conversion
//! three-vrm's normalised rig performs. The inverse, [`HumanoidRig::to_humanoid`], is `q = Wp · local · Wb⁻¹`.
//!
//! Optional humanoid bones a skeleton lacks are skipped: a rotation for an absent bone is ignored (as VRM Animation
//! specifies) and the children hang from the nearest present ancestor, which is what `Wp` of the child already is.
//! A *generic* skeleton (a quadruped, a blob: [`Skeleton::generic`]) lacks every humanoid bone, so there is nothing to
//! turn: [`HumanoidRig::new`] accepts it and every solve gives the rest pose (the soft parts move through the spring
//! solver as on any skeleton), while [`HumanoidRig::try_new`] refuses it for callers that need a humanoid.
//! Bones that are not humanoid (hair, cloth, accessories) stay at rest here; spring bones and IK write them later in
//! the frame (spec §8 order).
//!
//! The hips offset is in the same world-aligned rest axes, so it is carried into the hips parent's frame (`Wp⁻¹`)
//! before it is added to the hips translation.

use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone, HumanoidPose, NotHumanoid, Pose, Rig, Skeleton, Tick};
use omuzaki_core::glam::{Quat, Vec4};

use crate::skeleton::RigSkeleton;

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

impl Binding {
    fn new(rig: &RigSkeleton, bone: HumanoidBone) -> Option<Self> {
        let skeleton = rig.skeleton();
        let id = skeleton.humanoid_bone(bone)?;
        let parent_world = match skeleton.get(id)?.parent {
            Some(parent) => rig.rest_world(parent)?.rotation.normalize(),
            None => Quat::IDENTITY,
        };
        Some(Self {
            bone: bone.index(),
            joint: id.index(),
            parent_world,
            own_world: rig.rest_world(id)?.rotation.normalize(),
        })
    }
}

/// Solves humanoid poses onto one skeleton: the "humanoid rotations onto the skeleton's rest" stage of [`Rig::solve`].
///
/// It implements [`Rig`] on its own, so a character without hair physics works as it is; the full rig composes this
/// with IK and the spring solver, which write the bones this leaves at rest.
///
/// On a generic (non-humanoid) skeleton it has no bindings and passes the rest pose through; see the module docs.
#[derive(Clone, Debug)]
pub struct HumanoidRig {
    skeleton: Arc<RigSkeleton>,
    /// Every humanoid bone the skeleton has, in [`HumanoidBone::ALL`] order (none for a generic skeleton).
    bindings: Vec<Binding>,
    /// The hips: the only bone whose position the pose moves. `None` for a generic skeleton, which has no humanoid
    /// bones (a humanoid [`Skeleton`] always has hips).
    hips: Option<Binding>,
}

impl HumanoidRig {
    /// Builds the conversion tables for `skeleton`. Cold: allocates.
    ///
    /// Accepts a generic skeleton too, as a rig that leaves every bone at rest. Use [`HumanoidRig::try_new`] when a
    /// non-humanoid skeleton is a mistake rather than a character to animate some other way.
    pub fn new(skeleton: Arc<RigSkeleton>) -> Self {
        let bindings: Vec<Binding> = HumanoidBone::ALL.into_iter().filter_map(|b| Binding::new(&skeleton, b)).collect();
        let hips = bindings.iter().copied().find(|b| b.bone == HumanoidBone::Hips.index());
        Self { skeleton, bindings, hips }
    }

    /// As [`HumanoidRig::new`], but refuses a skeleton that is not a VRM humanoid.
    ///
    /// # Errors
    /// [`NotHumanoid`] if `skeleton` was built with [`Skeleton::generic`].
    pub fn try_new(skeleton: Arc<RigSkeleton>) -> Result<Self, NotHumanoid> {
        skeleton.skeleton().require_humanoid()?;
        Ok(Self::new(skeleton))
    }

    /// The prepared skeleton, shared with a [`Skinner`](crate::skin::Skinner).
    pub fn rig_skeleton(&self) -> &Arc<RigSkeleton> {
        &self.skeleton
    }

    /// Writes the local transform of every bone for `pose`: humanoid bones turned by the pose, everything else at
    /// rest. Hot path: one copy of the rest pose and two quaternion multiplies per humanoid bone; no allocation.
    ///
    /// A rotation that is not finite (or is zero) leaves that bone at rest, and a non-finite hips offset is ignored, so
    /// a bad input never reaches the renderer as a NaN matrix.
    pub fn apply_humanoid(&self, pose: &HumanoidPose, out: &mut Pose) {
        out.local[..self.skeleton.len()].copy_from_slice(self.skeleton.rest_local());
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
    }

    /// The inverse of [`apply_humanoid`](Self::apply_humanoid): reads the humanoid rotations and the hips offset back
    /// out of a solved pose, in normalised space. Bones the skeleton lacks come out as the identity. This is how a
    /// pose on the rig is saved to the pose library (RG-06) or compared across characters. Hot path.
    pub fn to_humanoid(&self, local: &Pose, out: &mut HumanoidPose) {
        *out = HumanoidPose::REST;
        for b in &self.bindings {
            let q = b.parent_world * local.local[b.joint].rotation * b.own_world.conjugate();
            out.rotations[b.bone] = unit(q).unwrap_or(Quat::IDENTITY);
        }
        if let Some(hips) = self.hips {
            let moved = local.local[hips.joint].translation - self.skeleton.rest_local()[hips.joint].translation;
            out.hips_offset = hips.parent_world * moved;
        }
    }
}

impl Rig for HumanoidRig {
    fn skeleton(&self) -> &Skeleton {
        self.skeleton.skeleton()
    }

    fn solve(&mut self, _tick: Tick, pose: &HumanoidPose, out: &mut Pose) {
        self.apply_humanoid(pose, out);
    }

    /// Nothing to clear: the humanoid solve is a pure function of the pose.
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::Transform;
    use omuzaki_core::glam::Vec3;

    use super::*;

    fn rig_with(bones: &[HumanoidBone]) -> HumanoidRig {
        let skeleton = Skeleton::humanoid(bones, |_| Transform::from_translation(Vec3::Y * 0.1)).unwrap();
        HumanoidRig::new(RigSkeleton::new(skeleton))
    }

    #[test]
    fn the_rest_pose_solves_to_the_rest_transforms() {
        let rig = rig_with(&HumanoidBone::ALL);
        let mut out = Pose::IDENTITY;
        rig.apply_humanoid(&HumanoidPose::REST, &mut out);
        for (got, want) in out.local.iter().zip(rig.rig_skeleton().rest_local()) {
            assert!(got.rotation.angle_between(want.rotation) < 1e-6);
            assert_eq!(got.translation, want.translation);
        }
    }

    #[test]
    fn bones_the_skeleton_lacks_are_ignored() {
        let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
        let rig = rig_with(&required);
        assert_eq!(rig.bindings.len(), 15);
        let mut pose = HumanoidPose::REST;
        pose.set(HumanoidBone::Chest, Quat::from_rotation_x(1.0)); // absent: no bone to turn
        pose.set(HumanoidBone::Head, Quat::from_rotation_y(0.5));
        let mut out = Pose::IDENTITY;
        rig.apply_humanoid(&pose, &mut out);
        let head = rig.rig_skeleton().skeleton().humanoid_bone(HumanoidBone::Head).unwrap();
        assert!(out.local[head.index()].rotation.angle_between(Quat::from_rotation_y(0.5)) < 1e-6);
        let mut back = HumanoidPose::REST;
        rig.to_humanoid(&out, &mut back);
        assert_eq!(back.get(HumanoidBone::Chest), Quat::IDENTITY, "an absent bone reads back as the identity");
    }

    #[test]
    fn garbage_input_leaves_bones_at_rest() {
        let rig = rig_with(&HumanoidBone::ALL);
        let mut pose = HumanoidPose::REST;
        pose.set(HumanoidBone::Head, Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0));
        pose.set(HumanoidBone::Neck, Quat::from_xyzw(0.0, 0.0, 0.0, 0.0));
        pose.hips_offset = Vec3::new(f32::INFINITY, 0.0, 0.0);
        let mut out = Pose::IDENTITY;
        rig.apply_humanoid(&pose, &mut out);
        assert!(out.local[..55].iter().all(|t| t.translation.is_finite() && t.rotation.is_finite()));
        let hips = rig.rig_skeleton().skeleton().humanoid_bone(HumanoidBone::Hips).unwrap();
        assert_eq!(out.local[hips.index()].translation, rig.rig_skeleton().rest_local()[hips.index()].translation);
    }

    #[test]
    fn the_rig_trait_solves_and_resets() {
        let mut rig = rig_with(&HumanoidBone::ALL);
        let mut pose = HumanoidPose::REST;
        pose.set(HumanoidBone::Hips, Quat::from_rotation_z(0.3));
        let (mut via_trait, mut direct) = (Pose::IDENTITY, Pose::IDENTITY);
        rig.solve(Tick::at_rate(7, 30.0), &pose, &mut via_trait);
        rig.apply_humanoid(&pose, &mut direct);
        assert_eq!(via_trait, direct);
        rig.reset();
        assert_eq!(Rig::skeleton(&rig).len(), 55);
    }
}
