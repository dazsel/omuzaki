//! Skinning matrices: a solved [`Pose`] to the joint palette the renderer takes (spec §12, `Scene::joints`).
//!
//! The renderer knows no skeleton; it wants one matrix per joint that maps the mesh's bind-pose space to model space,
//! `world(bone) × inverse_bind(bone)` (`omuzaki-render-api`, `Instance::skin`). [`Skinner`] produces them with
//! [`Skeleton::world_pose`](omuzaki_anim_api::Skeleton::world_pose) (the contract's forward-kinematics pass) over the inverse bind matrices that
//! [`RigSkeleton`] holds (computed from the rest pose, or given as data), and owns the scratch space that pass needs, so
//! a frame allocates nothing.

use std::sync::Arc;

use omuzaki_anim_api::{BoneId, Pose};
use omuzaki_core::glam::Mat4;
use omuzaki_render_api::JointRange;

use crate::skeleton::RigSkeleton;

/// Turns solved poses into skinning matrices for one character. One per character (and thread): it holds the
/// forward-kinematics scratch, while the skeleton it reads is shared.
#[derive(Clone, Debug)]
pub struct Skinner {
    skeleton: Arc<RigSkeleton>,
    /// Root-relative transforms of the last pose (16 KiB, so boxed rather than carried on the stack).
    world: Box<Pose>,
}

impl Skinner {
    /// A skinner for `skeleton`. Cold: allocates the scratch.
    pub fn new(skeleton: Arc<RigSkeleton>) -> Self {
        Self { skeleton, world: Box::new(Pose::IDENTITY) }
    }

    /// The skeleton this skinner reads.
    pub fn rig_skeleton(&self) -> &Arc<RigSkeleton> {
        &self.skeleton
    }

    /// Writes the skinning matrix of every bone for the local transforms in `local` into `out`, indexed by
    /// [`BoneId`](omuzaki_anim_api::BoneId). `out` should hold [`RigSkeleton::len`] matrices; a shorter slice is filled as
    /// far as it goes. Runs the forward-kinematics pass itself; a caller that already has the world pose uses
    /// [`skin_from_world`](Self::skin_from_world). Hot path: no allocation.
    pub fn skin(&mut self, local: &Pose, out: &mut [Mat4]) {
        self.skeleton.skeleton().world_pose(local, &mut self.world);
        Self::fill(&self.skeleton, &self.world, out);
    }

    /// As [`skin`](Self::skin), for a `world` pose the caller has already computed with [`Skeleton::world_pose`](omuzaki_anim_api::Skeleton::world_pose)
    /// (root-relative transforms of the pose being skinned), so the forward-kinematics pass is not run twice. Hot path:
    /// no allocation.
    pub fn skin_from_world(&self, world: &Pose, out: &mut [Mat4]) {
        Self::fill(&self.skeleton, world, out);
    }

    /// `world(bone) × inverse_bind(bone)` for each bone `out` has room for.
    fn fill(skeleton: &RigSkeleton, world: &Pose, out: &mut [Mat4]) {
        for ((slot, w), ib) in out.iter_mut().zip(&world.local[..skeleton.len()]).zip(skeleton.inverse_bind()) {
            *slot = w.to_mat4() * *ib;
        }
    }

    /// Appends the skinning matrices to a scene's joint palette and returns the range an
    /// [`Instance`](omuzaki_render_api::Instance) names in its `skin` field. The caller clears the palette once per
    /// frame (`Scene::clear`); once the palette has grown to its working size this allocates nothing.
    pub fn push(&mut self, local: &Pose, joints: &mut Vec<Mat4>) -> JointRange {
        let start = joints.len();
        joints.resize(start + self.skeleton.len(), Mat4::IDENTITY);
        self.skin(local, &mut joints[start..]);
        JointRange {
            start: u32::try_from(start).unwrap_or(u32::MAX),
            len: u32::try_from(self.skeleton.len()).unwrap_or(u32::MAX),
        }
    }

    /// Appends one mesh's joint run: for each `i`, `world(bones[i]) × inverse_bind[i]`. `world` is the final world pose of
    /// the frame (as [`skin_from_world`](Self::skin_from_world) takes it). Returns the run for
    /// [`Instance::skin`](omuzaki_render_api::Instance). A mesh has its own inverse bind matrices, so a joint may get a
    /// different matrix in each run that names it. Once the palette has grown to its working size this allocates nothing.
    ///
    /// The caller checks that `bones` and `inverse_bind` pair up. A debug build panics if they don't; a release build
    /// uses the shorter of the two.
    pub fn push_run_from_world(
        &self,
        world: &Pose,
        bones: &[BoneId],
        inverse_bind: &[Mat4],
        joints: &mut Vec<Mat4>,
    ) -> JointRange {
        debug_assert_eq!(
            bones.len(),
            inverse_bind.len(),
            "push_run_from_world: one inverse bind matrix per bone (the run uses the shorter of the two)"
        );
        let start = joints.len();
        joints.extend(bones.iter().zip(inverse_bind).map(|(bone, ib)| world.local[bone.index()].to_mat4() * *ib));
        JointRange {
            start: u32::try_from(start).unwrap_or(u32::MAX),
            len: u32::try_from(joints.len() - start).unwrap_or(u32::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::{HumanoidBone, Skeleton, Transform};
    use omuzaki_core::glam::{Quat, Vec3};

    use super::*;

    fn skinner() -> Skinner {
        let skeleton = Skeleton::humanoid(&HumanoidBone::ALL, |_| Transform::from_translation(Vec3::Y)).unwrap();
        Skinner::new(RigSkeleton::new(skeleton))
    }

    #[test]
    fn the_rest_pose_skins_to_identity() {
        let mut skinner = skinner();
        let rest = skinner.rig_skeleton().skeleton().rest_pose();
        let mut out = vec![Mat4::ZERO; 55];
        skinner.skin(&rest, &mut out);
        assert!(out.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)), "no pose, no skinning");
    }

    #[test]
    fn a_turned_bone_swings_everything_below_it() {
        let mut skinner = skinner();
        let mut pose = skinner.rig_skeleton().skeleton().rest_pose();
        let skeleton = skinner.rig_skeleton().skeleton().clone();
        let hips = skeleton.humanoid_bone(HumanoidBone::Hips).unwrap();
        pose.local[hips.index()].rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let mut out = vec![Mat4::ZERO; 55];
        skinner.skin(&pose, &mut out);
        // A vertex at the spine's bind position (0, 2, 0) turns 90 degrees about the hips at (0, 1, 0): to (-1, 1, 0).
        let spine = skeleton.humanoid_bone(HumanoidBone::Spine).unwrap();
        let moved = out[spine.index()].transform_point3(Vec3::new(0.0, 2.0, 0.0));
        assert!((moved - Vec3::new(-1.0, 1.0, 0.0)).length() < 1e-5, "{moved:?}");
    }

    #[test]
    fn push_appends_a_range_and_reuses_the_palette() {
        let mut skinner = skinner();
        let rest = skinner.rig_skeleton().skeleton().rest_pose();
        let mut joints = vec![Mat4::IDENTITY; 3];
        let first = skinner.push(&rest, &mut joints);
        assert_eq!(first, JointRange { start: 3, len: 55 });
        let second = skinner.push(&rest, &mut joints);
        assert_eq!(second, JointRange { start: 58, len: 55 });
        assert_eq!(joints.len(), 113);

        joints.clear();
        let capacity = joints.capacity();
        assert_eq!(skinner.push(&rest, &mut joints), JointRange { start: 0, len: 55 });
        assert_eq!(joints.capacity(), capacity, "a cleared palette keeps its storage");
    }
}
