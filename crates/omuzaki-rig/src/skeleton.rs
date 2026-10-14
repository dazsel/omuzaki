//! A skeleton prepared for solving and skinning: the cold, immutable tables both hot paths read.
//!
//! [`omuzaki_anim_api::Skeleton`] is the validated description (names, hierarchy, rest transforms). Solving a humanoid
//! pose needs each bone's rest orientation in *skeleton space*, and skinning needs the inverse bind matrices. The first
//! is derived from the rest pose alone. The second is **data** (decision 0014): by default it is the inverse of the
//! rest pose ([`RigSkeleton::new`]), but a character rests in the VRM T-pose and is bound in its sculpted pose, so
//! [`RigSkeleton::with_inverse_bind`] takes the matrices as given. Both are computed once here and shared (behind an
//! [`Arc`]) by every [`HumanoidRig`](crate::pose::HumanoidRig) and [`Skinner`](crate::skin::Skinner) of the same
//! character.

use std::sync::Arc;

use omuzaki_anim_api::{BoneId, Pose, Skeleton, Transform};
use omuzaki_core::glam::Mat4;

/// Why a table of inverse bind matrices doesn't belong to a skeleton ([`RigSkeleton::with_inverse_bind`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InverseBindError {
    /// There isn't exactly one matrix per bone.
    #[error("{given} inverse bind matrices for a skeleton of {expected} bones: it needs exactly one per bone")]
    Length {
        /// How many bones the skeleton has.
        expected: usize,
        /// How many matrices were given.
        given: usize,
    },
    /// A matrix has a NaN or infinite entry, which would poison every vertex the bone moves.
    #[error("the inverse bind matrix of bone {bone} ({name:?}) is not finite")]
    NotFinite {
        /// The bone's index.
        bone: usize,
        /// Its name.
        name: String,
    },
}

/// A [`Skeleton`] plus everything derived from its rest pose, and the inverse bind matrices it skins with.
#[derive(Clone, Debug)]
pub struct RigSkeleton {
    skeleton: Skeleton,
    rest_local: Vec<Transform>,
    rest_world: Vec<Transform>,
    inverse_bind: Vec<Mat4>,
}

impl RigSkeleton {
    /// Prepares `skeleton` for a mesh bound at rest: the inverse bind matrices are [`Skeleton::inverse_bind`]. Cold:
    /// loops the bones a few times and allocates, so do it when a character loads.
    pub fn new(skeleton: Skeleton) -> Arc<Self> {
        let inverse_bind = skeleton.inverse_bind();
        Self::prepared(skeleton, inverse_bind)
    }

    /// Prepares `skeleton` with inverse bind matrices that are not the rest pose's: a character that rests in the T-pose
    /// but whose mesh is bound in its sculpted pose (`Skeleton::inverse_bind_for`, `CharacterRig::inverse_bind`), or an
    /// imported model's own matrices, kept as given. Cold, like [`RigSkeleton::new`].
    ///
    /// # Errors
    /// [`InverseBindError`] if `inverse_bind` doesn't have one matrix per bone, or one of them isn't finite.
    pub fn with_inverse_bind(skeleton: Skeleton, inverse_bind: Vec<Mat4>) -> Result<Arc<Self>, InverseBindError> {
        if inverse_bind.len() != skeleton.len() {
            return Err(InverseBindError::Length { expected: skeleton.len(), given: inverse_bind.len() });
        }
        if let Some(bone) = inverse_bind.iter().position(|m| !m.is_finite()) {
            return Err(InverseBindError::NotFinite { bone, name: skeleton.bones()[bone].name.clone() });
        }
        Ok(Self::prepared(skeleton, inverse_bind))
    }

    /// The tables derived from the rest pose, with the given (already checked) inverse bind matrices.
    fn prepared(skeleton: Skeleton, inverse_bind: Vec<Mat4>) -> Arc<Self> {
        let rest_local: Vec<Transform> = skeleton.bones().iter().map(|b| b.rest).collect();
        let mut world = Pose::IDENTITY;
        skeleton.world_pose(&skeleton.rest_pose(), &mut world);
        let rest_world = world.local[..skeleton.len()].to_vec();
        Arc::new(Self { skeleton, rest_local, rest_world, inverse_bind })
    }

    /// The validated skeleton.
    pub fn skeleton(&self) -> &Skeleton {
        &self.skeleton
    }

    /// How many bones (and so how many skinning matrices a frame produces).
    pub fn len(&self) -> usize {
        self.skeleton.len()
    }

    /// True if there are no bones (never true for a valid skeleton).
    pub fn is_empty(&self) -> bool {
        self.skeleton.is_empty()
    }

    /// Every bone's rest transform relative to its parent, indexed by [`BoneId`]: one contiguous slice so a frame can
    /// start from the rest pose with a single copy.
    pub fn rest_local(&self) -> &[Transform] {
        &self.rest_local
    }

    /// The rest transform of `bone` relative to the skeleton root (its rest pose in model space; the bind pose is
    /// whatever [`RigSkeleton::inverse_bind`] inverts).
    pub fn rest_world(&self, bone: BoneId) -> Option<Transform> {
        self.rest_world.get(bone.index()).copied()
    }

    /// The inverse bind matrices, indexed by [`BoneId`]: [`Skeleton::inverse_bind`] for [`RigSkeleton::new`], the
    /// matrices given to [`RigSkeleton::with_inverse_bind`] otherwise. Computed or checked once.
    pub fn inverse_bind(&self) -> &[Mat4] {
        &self.inverse_bind
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
    use omuzaki_core::glam::{Quat, Vec3};

    use super::*;
    use crate::pose::HumanoidRig;
    use crate::skin::Skinner;

    fn rig(offset: Vec3) -> Arc<RigSkeleton> {
        let skeleton = Skeleton::humanoid(&HumanoidBone::ALL, |_| Transform::from_translation(offset)).unwrap();
        RigSkeleton::new(skeleton)
    }

    #[test]
    fn rest_world_accumulates_down_the_hierarchy() {
        let rig = rig(Vec3::Y);
        let at = |b| rig.rest_world(rig.skeleton().humanoid_bone(b).unwrap()).unwrap().translation;
        assert_eq!(at(HumanoidBone::Hips), Vec3::Y);
        assert_eq!(at(HumanoidBone::Spine), 2.0 * Vec3::Y);
        assert_eq!(at(HumanoidBone::Head), 6.0 * Vec3::Y, "hips, spine, chest, upperChest, neck, head");
        assert_eq!(rig.rest_world(BoneId(999)), None);
    }

    #[test]
    fn tables_cover_every_bone_and_agree_with_the_contract() {
        let rig = rig(Vec3::new(0.0, 0.1, 0.0));
        assert_eq!((rig.len(), rig.rest_local().len(), rig.inverse_bind().len()), (55, 55, 55));
        assert!(!rig.is_empty());
        assert_eq!(rig.inverse_bind(), rig.skeleton().inverse_bind().as_slice());
        // Bind pose times its inverse is the identity, bone by bone.
        for (i, ib) in rig.inverse_bind().iter().enumerate() {
            let world = rig.rest_world(BoneId(u16::try_from(i).unwrap())).unwrap().to_mat4();
            assert!((world * *ib).abs_diff_eq(Mat4::IDENTITY, 1e-5), "bone {i}");
        }
    }

    /// A humanoid whose bone axes are tilted differently on every bone (what a Blender or VRM 0.x export looks like), so
    /// "normalised" and "local" rotations differ and the tests exercise the real conversion.
    #[allow(clippy::cast_precision_loss)] // small bone indices
    fn tilted_humanoid() -> Skeleton {
        Skeleton::humanoid(&HumanoidBone::ALL, |bone| Transform {
            translation: Vec3::new(0.02 * (bone.index() % 3) as f32, 0.1, 0.01),
            rotation: Quat::from_rotation_y(0.2 * (bone.index() % 7) as f32)
                * Quat::from_rotation_x(0.1 * (bone.index() % 3) as f32),
        })
        .unwrap()
    }

    fn bits(matrices: &[Mat4]) -> Vec<u32> {
        matrices.iter().flat_map(Mat4::to_cols_array).map(f32::to_bits).collect()
    }

    #[test]
    fn inverse_bind_matrices_given_as_data_are_kept_exactly_and_skin_the_bind_pose_to_itself() {
        let skeleton = tilted_humanoid();
        let hand = |rig: &Arc<RigSkeleton>| HumanoidRig::new(Arc::clone(rig));
        let at_rest = RigSkeleton::new(skeleton.clone());
        assert_eq!(
            bits(at_rest.inverse_bind()),
            bits(&skeleton.inverse_bind()),
            "`new` binds at rest, as it always has"
        );

        // The mesh was sculpted with the left arm lowered and the forearm bent: a bind pose in normalised space.
        let mut bind = HumanoidPose::REST;
        bind.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(-1.0));
        bind.set(HumanoidBone::LeftLowerArm, Quat::from_rotation_x(0.4));
        let mut bound = Pose::IDENTITY;
        hand(&at_rest).apply_humanoid(&bind, &mut bound);
        let given = skeleton.inverse_bind_for(&bound);
        let rig = RigSkeleton::with_inverse_bind(skeleton.clone(), given.clone()).unwrap();
        assert_eq!(bits(rig.inverse_bind()), bits(&given), "kept as given");
        assert_eq!(rig.rest_local(), at_rest.rest_local(), "the rest tables don't depend on the bind");
        assert_eq!(rig.rest_world(BoneId(5)), at_rest.rest_world(BoneId(5)));

        let mut skinner = Skinner::new(Arc::clone(&rig));
        let mut skin = vec![Mat4::ZERO; rig.len()];
        skinner.skin(&bound, &mut skin);
        assert!(
            skin.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)),
            "posed at its bind pose the mesh is as built"
        );

        // Posed at rest (the T), each bone moves its vertices by rest world * inverse bind world; bones the bind didn't
        // touch stay put and the lowered arm and everything under it swings out.
        let mut rest = Pose::IDENTITY;
        hand(&rig).apply_humanoid(&HumanoidPose::REST, &mut rest);
        skinner.skin(&rest, &mut skin);
        let (mut bind_world, mut rest_world) = (Pose::IDENTITY, Pose::IDENTITY);
        skeleton.world_pose(&bound, &mut bind_world);
        skeleton.world_pose(&rest, &mut rest_world);
        for (i, m) in skin.iter().enumerate() {
            let expected = rest_world.local[i].to_mat4() * bind_world.local[i].to_mat4().inverse();
            assert!(m.abs_diff_eq(expected, 1e-4), "bone {i}");
        }
        let id = |b| skeleton.humanoid_bone(b).unwrap().index();
        assert!(skin[id(HumanoidBone::Hips)].abs_diff_eq(Mat4::IDENTITY, 1e-5));
        assert!(!skin[id(HumanoidBone::LeftUpperArm)].abs_diff_eq(Mat4::IDENTITY, 0.1));
    }

    #[test]
    fn a_table_of_the_wrong_size_or_with_a_bad_number_is_refused_naming_the_bone() {
        let skeleton = tilted_humanoid();
        let good = skeleton.inverse_bind();

        let short = good[..54].to_vec();
        let error = RigSkeleton::with_inverse_bind(skeleton.clone(), short).unwrap_err();
        assert_eq!(error, InverseBindError::Length { expected: 55, given: 54 });
        assert!(error.to_string().contains("54") && error.to_string().contains("55"), "{error}");
        let long = [good.clone(), vec![Mat4::IDENTITY]].concat();
        assert_eq!(
            RigSkeleton::with_inverse_bind(skeleton.clone(), long).unwrap_err(),
            InverseBindError::Length { expected: 55, given: 56 }
        );

        let mut bad = good;
        bad[7].w_axis.y = f32::NAN;
        let name = skeleton.bones()[7].name.clone();
        let error = RigSkeleton::with_inverse_bind(skeleton.clone(), bad.clone()).unwrap_err();
        assert_eq!(error, InverseBindError::NotFinite { bone: 7, name: name.clone() });
        assert!(error.to_string().contains(&name), "{error}");
        bad[7].w_axis.y = f32::INFINITY;
        assert!(matches!(
            RigSkeleton::with_inverse_bind(skeleton, bad),
            Err(InverseBindError::NotFinite { bone: 7, .. })
        ));
    }
}
