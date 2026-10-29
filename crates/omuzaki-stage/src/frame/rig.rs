//! Pose to skeleton to skinning matrices: the rig solve for one frame (humanoid pose, then IK, then the character's
//! bone drivers, then the arm guard, then spring bones, then one forward-kinematics pass) and the [`PoseSource`] the
//! part runtimes follow it with.
//!
//! The arm guard (#2353) keeps the forearms and hands out of the body whatever posed them. Its body is measured from
//! the character's own skinned meshes, brought to the rest pose through their inverse binds, so a kit character and an
//! imported avatar each get theirs; a body it cannot measure (no humanoid arms, no torso in the meshes) runs without.
//!
//! The bone drivers ([`DriverPlan`], #800) run before the springs, where VRM puts node constraints: a spring chain
//! under a driven bone (feathers on a folding wing) follows it in the same frame, and an exported `Follow` driver moves
//! in a VRM viewer as it does here.

use std::sync::Arc;

use omuzaki_anim_api::{
    BoneId, ChannelBuf, ConstraintSet, HumanoidPose, IkGoals, IkSolver, MAX_BONES, Pose, Reaches, Rig, Skeleton,
    SpringSet, Tick,
};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_char_api::{BuiltCharacter, CharacterRig, DriverPlan, JointRef, PartMesh, PoseSource};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_rig::constraint::VrmConstraintSolver;
use omuzaki_rig::full::FullRig;
use omuzaki_rig::ik::guard::{
    ArmGuard, BodyPart, BodyProxy, GuardConfig, MAX_OBSTACLES, Obstacle, SkinnedMesh,
};
use omuzaki_rig::ik::TwoBoneIk;
use omuzaki_rig::ik::look_at::LookAtIk;
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::spring::VrmSpringSolver;

use crate::error::StageError;
use crate::interact::{BODY_JOINTS, MemberBody};

/// Both IK solvers as one stage. The gaze goes first because the look-at turns the chest and neck, which carry the
/// arms: the limbs then solve against that posed upper body, so a hand or foot goal still lands on its target when the
/// head looks away. Each solver ignores the other's goals, so one `IkGoals` drives both.
#[derive(Debug, Default)]
struct LimbsAndGaze {
    limbs: TwoBoneIk,
    gaze: LookAtIk,
}

impl IkSolver for LimbsAndGaze {
    fn solve(&mut self, skeleton: &Skeleton, goals: &IkGoals, pose: &mut Pose) {
        self.gaze.solve(skeleton, goals, pose);
        self.limbs.solve(skeleton, goals, pose);
    }

    fn reset(&mut self) {
        self.limbs.reset();
        self.gaze.reset();
    }
}

/// A character's composed rig with the buffers one frame needs, all sized once at construction.
pub(crate) struct RigFrame {
    rig: FullRig<LimbsAndGaze, VrmSpringSolver>,
    /// Local transforms of the last solve (boxed: the pose is 16 KiB).
    local: Box<Pose>,
    /// One skinning matrix per bone, bind space to model space.
    skin: Vec<Mat4>,
    /// The character's bone drivers, compiled once.
    drivers: DriverPlan,
    /// The channel values the drivers read: the last ones given to [`solve_with_channels`](Self::solve_with_channels),
    /// so a redraw or a still poses the driven bones as the last frame did (boxed: 512 bytes, and the stage is moved by value).
    channels: Box<ChannelBuf>,
}

impl RigFrame {
    /// Builds the rig for `character`: skinning against its bind pose, springs from its spring set, its bone drivers
    /// compiled. Cold.
    ///
    /// # Errors
    /// [`StageError::Rig`] if the inverse bind matrices, the spring set or the bone drivers don't fit the skeleton.
    pub(crate) fn new(character: &CharacterRig) -> Result<Self, StageError> {
        let rig_error = |reason: String| StageError::Rig { reason };
        let skeleton = RigSkeleton::with_inverse_bind(character.skeleton.clone(), character.inverse_bind())
            .map_err(|e| rig_error(e.to_string()))?;
        let bones = skeleton.len();
        let springs =
            VrmSpringSolver::new(&character.springs, skeleton.skeleton()).map_err(|e| rig_error(e.to_string()))?;
        let drivers = character.driver_plan().map_err(|e| rig_error(e.to_string()))?;
        let rig = FullRig::from_parts(skeleton, LimbsAndGaze::default(), Some(springs));
        Ok(Self {
            rig,
            local: Box::new(Pose::IDENTITY),
            skin: vec![Mat4::IDENTITY; bones.min(MAX_BONES)],
            drivers,
            channels: Box::new(ChannelBuf::NEUTRAL),
        })
    }

    /// Builds the rig for a body that brings its own skeleton (an imported avatar): springs and node constraints from
    /// its sets. Its meshes carry their own inverse binds, so the skin here is against the skeleton's rest and readers
    /// skin from [`world`](Self::world). Cold.
    ///
    /// # Errors
    /// [`StageError::Rig`] if the spring or constraint set doesn't fit the skeleton.
    pub(crate) fn for_skeleton(
        skeleton: &Skeleton,
        springs: &SpringSet,
        constraints: &ConstraintSet,
    ) -> Result<Self, StageError> {
        let rig_error = |reason: String| StageError::Rig { reason };
        let skeleton = RigSkeleton::new(skeleton.clone());
        let bones = skeleton.len();
        let springs = if springs.chains.is_empty() {
            None
        } else {
            Some(VrmSpringSolver::new(springs, skeleton.skeleton()).map_err(|e| rig_error(e.to_string()))?)
        };
        let mut rig = FullRig::from_parts(skeleton.clone(), LimbsAndGaze::default(), springs);
        if !constraints.constraints.is_empty() {
            let solver =
                VrmConstraintSolver::new(constraints, skeleton.skeleton()).map_err(|e| rig_error(e.to_string()))?;
            rig = rig.with_constraints(solver);
        }
        Ok(Self {
            rig,
            local: Box::new(Pose::IDENTITY),
            skin: vec![Mat4::IDENTITY; bones.min(MAX_BONES)],
            drivers: DriverPlan::default(),
            channels: Box::new(ChannelBuf::NEUTRAL),
        })
    }

    /// [`new`](Self::new) for a built kit character, with the arm guard measured from its meshes. Cold.
    ///
    /// # Errors
    /// As [`new`](Self::new).
    pub(crate) fn for_character(built: &BuiltCharacter) -> Result<Self, StageError> {
        let mut frame = Self::new(&built.rig)?;
        let inverse_bind = built.rig.inverse_bind();
        let meshes = built.meshes.iter().map(|m| (&m.mesh, None));
        let by_bone = |_, bone: BoneId| inverse_bind.get(bone.index()).copied();
        let bind = frame.bind_pose(&built.rig.bind);
        frame.rig.set_guard(arm_guard(&built.rig.skeleton, &bind, meshes, by_bone));
        Ok(frame)
    }

    /// Adds the arm guard measured from an imported avatar's meshes. Cold.
    #[must_use]
    pub(crate) fn guarding(mut self, avatar: &ImportedAvatar) -> Self {
        let meshes = avatar.meshes.iter().map(|m| (&m.mesh, Some(m.inverse_bind.as_slice())));
        let bind = self.bind_pose(&avatar.bind);
        self.rig.set_guard(arm_guard(&avatar.skeleton, &bind, meshes, |_, _| None));
        self
    }

    /// `bind` as local transforms on this rig's skeleton.
    fn bind_pose(&self, bind: &HumanoidPose) -> Pose {
        let mut pose = Pose::IDENTITY;
        HumanoidRig::new(Arc::clone(self.rig.rig_skeleton())).apply_humanoid(bind, &mut pose);
        pose
    }

    /// Solves `pose` through the rig with `goals` and refreshes the skinning matrices; the bone drivers read the last
    /// channels given. Hot path: no allocation.
    pub(crate) fn solve(&mut self, tick: Tick, pose: &HumanoidPose, goals: &IkGoals) {
        let Self { rig, local, skin, drivers, channels } = self;
        rig.set_goals(goals);
        if drivers.is_empty() {
            rig.solve_and_skin(tick, pose, local, skin);
        } else {
            rig.solve_and_skin_with(tick, pose, local, skin, |_, pose| drivers.apply(channels, pose));
        }
    }

    /// [`solve`](Self::solve) with this frame's `channels` for the bone drivers. Hot path: no allocation.
    pub(crate) fn solve_with_channels(&mut self, tick: Tick, pose: &HumanoidPose, goals: &IkGoals, channels: &ChannelBuf) {
        self.set_channels(channels);
        self.solve(tick, pose, goals);
    }

    /// The hands' reaches for the following solves, until replaced: each becomes a hand goal on the posed body
    /// (`omuzaki_rig::ik::reach`).
    pub(crate) fn set_reaches(&mut self, reaches: &Reaches) {
        self.rig.set_reaches(reaches);
    }

    /// The channel values the bone drivers read on the next [`solve`](Self::solve).
    pub(crate) fn channels(&self) -> &ChannelBuf {
        &self.channels
    }

    /// Sets the channel values the bone drivers read on the next [`solve`](Self::solve), without solving.
    pub(crate) fn set_channels(&mut self, channels: &ChannelBuf) {
        *self.channels = *channels;
    }

    /// Clears what the IK solvers and the spring bones carry from earlier frames (velocities, owed time); the goals
    /// stay. The next solve starts the springs from rest.
    pub(crate) fn reset(&mut self) {
        self.rig.reset();
    }

    /// The skinning matrices of the last [`solve`](Self::solve), indexed by bone.
    pub(crate) fn skin(&self) -> &[Mat4] {
        &self.skin
    }

    /// Every bone's model-space transform as of the last [`solve`](Self::solve), indexed by bone.
    pub(crate) fn world(&self) -> &Pose {
        self.rig.world()
    }

    /// The body as another member's interaction reads it (#2415): its joints and its guard's parts as the last solve
    /// left them, and its arms' sizes, carried into the world by `root`. Allocation-free.
    pub(crate) fn body(&self, root: Mat4) -> MemberBody {
        use omuzaki_anim_api::HumanoidBone as B;
        let skeleton = self.rig.skeleton();
        let world = self.rig.world();
        let mut body = MemberBody { root, ..MemberBody::EMPTY };
        if let Some(map) = skeleton.humanoid_map() {
            for (slot, bone) in body.joints.iter_mut().zip(BODY_JOINTS) {
                *slot = map
                    .get(bone)
                    .and_then(|id| world.local.get(id.index()))
                    .map(|t| root.transform_point3(t.translation));
            }
        }
        if let Some(guard) = self.rig.guard() {
            let proxy = guard.proxy();
            for (slot, part) in body.parts.iter_mut().zip(proxy.obstacles(skeleton, &self.local)) {
                *slot = Some(part.moved(root));
            }
            body.forearm = proxy.forearm_radius;
            body.palm = proxy.hands.map(Vec3::length);
            body.hand_thickness = proxy.hand_axes.map(Vec3::min_element);
        }
        let length = |a: Option<Vec3>, b: Option<Vec3>| a.zip(b).map_or(0.0, |(a, b)| a.distance(b));
        let j = |bone| body.joint(bone);
        body.arm = [
            length(j(B::LeftUpperArm), j(B::LeftLowerArm)) + length(j(B::LeftLowerArm), j(B::LeftHand)),
            length(j(B::RightUpperArm), j(B::RightLowerArm)) + length(j(B::RightLowerArm), j(B::RightHand)),
        ];
        body
    }

    /// Keeps the arms out of another member's parts too from the next solve: `parts` in the world, `root` this body's.
    /// An empty slice clears them. Allocation-free.
    pub(crate) fn set_partner(&mut self, parts: &[Obstacle], root: Mat4) {
        let Some(guard) = self.rig.guard_mut() else { return };
        let to_root = root.inverse();
        let mut placed = [EMPTY_OBSTACLE; MAX_OBSTACLES];
        for (slot, part) in placed.iter_mut().zip(parts) {
            *slot = part.moved(to_root);
        }
        guard.set_obstacles(&placed[..parts.len().min(MAX_OBSTACLES)]);
    }
}

/// A placeholder obstacle for the unused slots of a fixed buffer; never handed to the guard.
const EMPTY_OBSTACLE: Obstacle =
    Obstacle { part: BodyPart::Torso, a: Vec3::ZERO, b: Vec3::ZERO, radius: 0.0, depth: 0.0, forward: Vec3::Z };

/// The solved skeleton as the part runtimes see it: skin matrices looked up by joint, and the character's root.
pub(crate) struct SkinPose<'a> {
    pub(crate) character: &'a CharacterRig,
    pub(crate) skin: &'a [Mat4],
    pub(crate) root: Mat4,
}

impl PoseSource for SkinPose<'_> {
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4> {
        // The skin matrices are model space; a runtime simulates in world space, under the character's root.
        let bone = self.character.bone(joint)?;
        self.skin.get(bone.index()).map(|skin| self.root * *skin)
    }

    fn root(&self) -> Mat4 {
        self.root
    }
}

/// The bone a mesh joint means on `skeleton`.
fn bone_of(skeleton: &Skeleton, joint: &JointRef) -> Option<BoneId> {
    match joint {
        JointRef::Humanoid(h) => skeleton.humanoid_bone(*h),
        JointRef::Named(name) => skeleton.find(name.as_str()),
    }
}

/// The arm guard for a body with these skinned `meshes` (#2353), or `None` if it cannot be measured. Each mesh comes
/// with its own inverse bind matrices by joint, if it has them; otherwise `by_bone` gives the inverse bind of a bone.
/// The body is then made clear of the arms in `bind`, the pose the character rests in. Cold.
fn arm_guard<'a>(
    skeleton: &Skeleton,
    bind: &Pose,
    meshes: impl Iterator<Item = (&'a PartMesh, Option<&'a [Mat4]>)>,
    by_bone: impl Fn(usize, BoneId) -> Option<Mat4>,
) -> Option<ArmGuard> {
    let rest = skeleton.rest_pose();
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&rest, &mut world);
    let mut vertices = Vec::new();
    for (mesh, inverse_bind) in meshes.filter(|(m, _)| !m.dynamic && !m.joint_bones.is_empty()) {
        let bones: Vec<Option<BoneId>> = mesh.joint_bones.iter().map(|j| bone_of(skeleton, j)).collect();
        let g = &mesh.geometry;
        let skinned = SkinnedMesh { positions: &g.positions, joints: &g.joints, weights: &g.weights, bones: &bones };
        let unbind = |joint: usize, bone: BoneId| match inverse_bind {
            Some(matrices) => Some(matrices.get(joint).copied().unwrap_or(Mat4::IDENTITY)),
            None => by_bone(joint, bone),
        };
        BodyProxy::gather(&world, skinned, unbind, &mut vertices);
    }
    let config = GuardConfig::default();
    let mut proxy = BodyProxy::fit(skeleton, &rest, &vertices)?;
    proxy.clear_of(skeleton, bind, config.margin);
    Some(ArmGuard::new(proxy, config))
}

#[cfg(test)]
mod tests {
    use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};

    use super::*;

    /// The kit's mock character is a torso and a head with no arms: it cannot be measured, so it runs without a guard
    /// and solves as before. The real kit is measured and guarded in `omuzaki-test-char` (`tests/arm_guard_kit.rs`).
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn a_body_without_arms_runs_without_a_guard() {
        let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
        let built = RegistryBuilder
            .build(&CharId::new("kit").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
            .expect("the mock character builds");
        let mut frame = RigFrame::for_character(&built).unwrap();
        assert!(frame.rig.guard().is_none());
        frame.solve(Tick::at_rate(0, 60.0), &HumanoidPose::REST, &IkGoals::NONE);
    }
}
