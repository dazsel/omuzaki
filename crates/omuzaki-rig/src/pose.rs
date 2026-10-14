//! The humanoid pose solve: VRM normalised rotations onto one skeleton's own bones (spec §8, §20, RG-01).
//!
//! The conversion itself (`local = Wp⁻¹ · q · Wb`, and its inverse) lives in
//! [`HumanoidFrames`] in the contract crate, so `omuzaki-io-ops` can read an authored rest back as a pose without
//! depending on this crate; [`HumanoidRig`] holds one and forwards to it. See that module for the maths.
//!
//! A *generic* skeleton (a quadruped, a blob: [`Skeleton::generic`]) lacks every humanoid bone, so there is nothing to
//! turn: [`HumanoidRig::new`] accepts it and every solve gives the rest pose (the soft parts move through the spring
//! solver as on any skeleton), while [`HumanoidRig::try_new`] refuses it for callers that need a humanoid.
//! Bones that are not humanoid (hair, cloth, accessories) stay at rest here; spring bones and IK write them later in
//! the frame (spec §8 order).

use std::sync::Arc;

use omuzaki_anim_api::{HumanoidFrames, HumanoidPose, NotHumanoid, Pose, Rig, Skeleton, Tick};

use crate::skeleton::RigSkeleton;

/// Solves humanoid poses onto one skeleton: the "humanoid rotations onto the skeleton's rest" stage of [`Rig::solve`].
///
/// It implements [`Rig`] on its own, so a character without hair physics works as it is; the full rig composes this
/// with IK and the spring solver, which write the bones this leaves at rest.
///
/// On a generic (non-humanoid) skeleton it has no bindings and passes the rest pose through; see the module docs.
#[derive(Clone, Debug)]
pub struct HumanoidRig {
    skeleton: Arc<RigSkeleton>,
    /// The `Wp`/`Wb` table and both conversions; the maths lives in the contract crate.
    frames: HumanoidFrames,
}

impl HumanoidRig {
    /// Builds the conversion tables for `skeleton`. Cold: allocates.
    ///
    /// Accepts a generic skeleton too, as a rig that leaves every bone at rest. Use [`HumanoidRig::try_new`] when a
    /// non-humanoid skeleton is a mistake rather than a character to animate some other way.
    pub fn new(skeleton: Arc<RigSkeleton>) -> Self {
        let frames = HumanoidFrames::new(skeleton.skeleton());
        Self { skeleton, frames }
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
        self.frames.apply_humanoid(self.skeleton.rest_local(), pose, out);
    }

    /// The inverse of [`apply_humanoid`](Self::apply_humanoid): reads the humanoid rotations and the hips offset back
    /// out of a solved pose, in normalised space. Bones the skeleton lacks come out as the identity. This is how a
    /// pose on the rig is saved to the pose library (RG-06) or compared across characters. Hot path.
    pub fn to_humanoid(&self, local: &Pose, out: &mut HumanoidPose) {
        self.frames.to_humanoid(self.skeleton.rest_local(), local, out);
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
    use omuzaki_anim_api::{HumanoidBone, Transform};
    use omuzaki_core::glam::Quat;
    use omuzaki_core::glam::Vec3;
    use omuzaki_core::rng::Rng;

    use super::*;

    fn rig_with(bones: &[HumanoidBone]) -> HumanoidRig {
        let skeleton = Skeleton::humanoid(bones, |_| Transform::from_translation(Vec3::Y * 0.1)).unwrap();
        HumanoidRig::new(RigSkeleton::new(skeleton))
    }

    /// A Blender-style rig: every bone has its own arbitrary rest rotation and translation.
    fn arbitrary_rest_rig() -> HumanoidRig {
        let rng = std::cell::RefCell::new(Rng::new(99));
        let skeleton = Skeleton::humanoid(&HumanoidBone::ALL, |_| {
            let mut rng = rng.borrow_mut();
            Transform { translation: random_vec(&mut rng, 0.2), rotation: random_rotation(&mut rng) }
        })
        .unwrap();
        HumanoidRig::new(RigSkeleton::new(skeleton))
    }

    fn random_vec(rng: &mut Rng, spread: f32) -> Vec3 {
        Vec3::new(rng.range_f32(-spread, spread), rng.range_f32(-spread, spread), rng.range_f32(-spread, spread))
    }

    fn random_rotation(rng: &mut Rng) -> Quat {
        let axis = random_vec(rng, 1.0).try_normalize().unwrap_or(Vec3::Y);
        Quat::from_axis_angle(axis, rng.range_f32(-1.5, 1.5))
    }

    /// FNV-1a over the bit patterns of every number the two conversions produce for 8 seeded poses.
    fn conversion_digest(rig: &HumanoidRig) -> u64 {
        let mut rng = Rng::new(7);
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        let mix = |h: u64, v: f32| (h ^ u64::from(v.to_bits())).wrapping_mul(0x0000_0100_0000_01b3);
        for _ in 0..8 {
            let mut pose = HumanoidPose::REST;
            for bone in HumanoidBone::ALL {
                pose.set(bone, random_rotation(&mut rng));
            }
            pose.hips_offset = random_vec(&mut rng, 0.3);
            let (mut local, mut back) = (Pose::IDENTITY, HumanoidPose::REST);
            rig.apply_humanoid(&pose, &mut local);
            rig.to_humanoid(&local, &mut back);
            for t in &local.local[..rig.rig_skeleton().len()] {
                digest = t.translation.to_array().into_iter().chain(t.rotation.to_array()).fold(digest, mix);
            }
            digest ^= back.checksum();
        }
        digest
    }

    /// Recorded at `origin/main` before the maths moved to `HumanoidFrames`: the move must change no bit.
    #[test]
    fn the_conversions_are_bit_for_bit_what_they_were() {
        assert_eq!(conversion_digest(&arbitrary_rest_rig()), 13_635_560_173_654_275_620);
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
        assert_eq!(rig.frames.len(), 15);
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
