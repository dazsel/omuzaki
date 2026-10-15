//! Pose to skeleton to skinning matrices: the rig solve for one frame (humanoid pose, then IK, then spring bones, then
//! one forward-kinematics pass) and the [`PoseSource`] the part runtimes follow it with.

use omuzaki_anim_api::{HumanoidPose, IkGoals, IkSolver, MAX_BONES, Pose, Rig, Skeleton, Tick};
use omuzaki_char_api::{CharacterRig, JointRef, PoseSource};
use omuzaki_core::glam::Mat4;
use omuzaki_rig::full::FullRig;
use omuzaki_rig::ik::TwoBoneIk;
use omuzaki_rig::ik::look_at::LookAtIk;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::spring::VrmSpringSolver;

use crate::error::StageError;

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
}

impl RigFrame {
    /// Builds the rig for `character`: skinning against its bind pose, springs from its spring set. Cold.
    ///
    /// # Errors
    /// [`StageError::Rig`] if the inverse bind matrices or the spring set don't fit the skeleton.
    pub(crate) fn new(character: &CharacterRig) -> Result<Self, StageError> {
        let rig_error = |reason: String| StageError::Rig { reason };
        let skeleton = RigSkeleton::with_inverse_bind(character.skeleton.clone(), character.inverse_bind())
            .map_err(|e| rig_error(e.to_string()))?;
        let bones = skeleton.len();
        let springs =
            VrmSpringSolver::new(&character.springs, skeleton.skeleton()).map_err(|e| rig_error(e.to_string()))?;
        let rig = FullRig::from_parts(skeleton, LimbsAndGaze::default(), Some(springs));
        Ok(Self { rig, local: Box::new(Pose::IDENTITY), skin: vec![Mat4::IDENTITY; bones.min(MAX_BONES)] })
    }

    /// Solves `pose` through the rig with `goals` and refreshes the skinning matrices. Hot path: no allocation.
    pub(crate) fn solve(&mut self, tick: Tick, pose: &HumanoidPose, goals: &IkGoals) {
        self.rig.set_goals(goals);
        self.rig.solve_and_skin(tick, pose, &mut self.local, &mut self.skin);
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
}

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
