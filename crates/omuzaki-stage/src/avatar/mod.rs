//! [`AvatarStage`]: an imported avatar (a VRM or glTF a person brings, spec v3 §27 EX-12) on a renderer, with its frame
//! loop (§8; `docs/design/byom.md` D1 to D4, ADR 0033).
//!
//! The kit's [`Stage`](crate::frame::Stage) runs a built character with its painted face and part runtimes. An imported
//! avatar has neither, and brings what a built character lacks: its own inverse binds per mesh, morph targets, node
//! constraints and expressions. [`AvatarStage::tick`] runs one frame in the §8 order:
//!
//! 1. the stage clock gives the frame's tick (fixed or live, as for the kit's stage);
//! 2. on the bone gaze route, the `eyeLook*` channels turn the eye bones ([`gaze`]);
//! 3. the pose goes through the rig: humanoid pose, IK and look-at, node constraints, spring bones;
//! 4. the expression evaluator (`omuzaki-avatar`) turns the channels into morph weights and material binds, and a
//!    material a bind moved is re-sent ([`upload`]);
//! 5. the scene gets one instance per mesh, its joint run (each bone's model-space matrix times the mesh's own inverse
//!    bind) and the morph weights ([`draw`]), and is rendered.
//!
//! After construction nothing in steps 1 to 5 allocates in stage-owned code; the renderer costs what it costs.

mod draw;
mod gaze;
pub mod upload;

use omuzaki_anim_api::{BoneId, HumanoidPose, IkGoals, Tick};
use omuzaki_avatar::buffers::{BindDeltas, MorphWeights};
use omuzaki_avatar::evaluator::ExpressionEvaluator;
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_core::glam::Mat4;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{FrameInfo, Quality, Renderer, Scene, TargetId};

use self::draw::AvatarPlan;
use self::gaze::EyeGaze;
use self::upload::AvatarUpload;
use crate::error::StageError;
use crate::frame::rig::RigFrame;
use crate::frame::{FrameInput, FrameOutput, StageClock};
use crate::scene::apply::{Owed, SceneLink, reserve};
use crate::scene::resolve::ResolvedScene;

/// An imported avatar on a renderer with everything its frame loop needs.
pub struct AvatarStage {
    avatar: ImportedAvatar,
    upload: AvatarUpload,
    rig: RigFrame,
    plan: AvatarPlan,
    evaluator: ExpressionEvaluator,
    weights: MorphWeights,
    binds: BindDeltas,
    gaze: Option<EyeGaze>,
    /// The pose of the last frame, the gaze turned into it: what a redraw shows again.
    pose: HumanoidPose,
    goals: IkGoals,
    /// The avatar's world matrix of the last frame: the subject's root times the input's.
    root: Mat4,
    input_root: Mat4,
    subject_root: Mat4,
    link: SceneLink,
    submissions: u64,
    tick: Tick,
    scene: Scene,
    target: TargetId,
    quality: Quality,
    clock: StageClock,
}

impl AvatarStage {
    /// Checks `avatar`, uploads it to `renderer` and prepares its frame loop. `scene` carries what the loop does not
    /// own (camera, lights, background, post settings); `target` is where frames render. Cold: allocates.
    ///
    /// # Errors
    /// [`StageError::Avatar`] for an avatar that fails [`ImportedAvatar::validate`], any [`AvatarUpload::upload`]
    /// error, [`StageError::Rig`] for springs or constraints that don't fit the skeleton. On an error nothing is left
    /// on the renderer.
    pub fn new(
        renderer: &mut dyn Renderer,
        avatar: ImportedAvatar,
        scene: Scene,
        target: TargetId,
        quality: Quality,
        clock: StageClock,
    ) -> Result<Self, StageError> {
        avatar.validate().map_err(|e| StageError::Avatar { reason: e.to_string() })?;
        let evaluator = ExpressionEvaluator::new(&avatar);
        let weights = evaluator.morph_weights();
        let binds = evaluator.bind_deltas();
        let upload = AvatarUpload::upload(renderer, &avatar, &binds)?;
        let prepared = RigFrame::for_skeleton(&avatar.skeleton, &avatar.springs, &avatar.constraints)
            .and_then(|rig| AvatarPlan::new(&avatar, &upload, &weights).map(|plan| (rig, plan)));
        let (rig, plan) = match prepared {
            Ok(parts) => parts,
            Err(error) => {
                if let Err(cleanup) = upload.release(renderer) {
                    tracing::warn!(%cleanup, "releasing an avatar whose stage could not start failed too");
                }
                return Err(error);
            }
        };
        let mut scene = scene;
        reserve(&mut scene);
        plan.reserve(&mut scene);
        Ok(Self {
            gaze: EyeGaze::of(&avatar),
            pose: avatar.bind,
            avatar,
            upload,
            rig,
            plan,
            evaluator,
            weights,
            binds,
            goals: IkGoals::NONE,
            root: Mat4::IDENTITY,
            input_root: Mat4::IDENTITY,
            subject_root: Mat4::IDENTITY,
            link: SceneLink::new(),
            submissions: 0,
            tick: Tick { index: 0, time: Seconds(0.0), dt: 0.0 },
            scene,
            target,
            quality,
            clock,
        })
    }

    /// The pose the avatar's meshes were made in: what a performer or animator layers onto, so that with nothing
    /// animating the avatar stands as its file has it (decision 0014 §4, as the kit's `seed_base_pose`).
    pub fn base_pose(&self) -> HumanoidPose {
        self.avatar.bind
    }

    /// Runs one frame and renders it into the stage's target. Returns the frame, with
    /// [`uploads`](FrameOutput::uploads) counting the materials an expression bind re-sent.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a material update or the frame. The rig has already stepped; the
    /// next call carries on from there.
    pub fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        let tick = self.clock.next(input.dt, input.time);
        self.tick = tick;
        self.input_root = input.root;
        self.root = self.subject_root * input.root;
        self.pose = input.pose;
        if let Some(gaze) = &self.gaze {
            gaze.apply(&input.channels, &mut self.pose);
        }
        self.goals = input.goals;
        self.rig.solve(tick, &self.pose, &self.goals);
        self.draw(renderer, input, tick, "the frame")
    }

    /// Renders the pose the last [`tick`](Self::tick) left with `input`'s channels, for a still that differs from the
    /// last frame only by its camera or expression: the clock and the rig do not move.
    ///
    /// # Errors
    /// As [`tick`](Self::tick).
    pub fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        self.draw(renderer, input, self.tick, "the still")
    }

    /// Steps 4 and 5: expressions, the scene, the render.
    fn draw(
        &mut self,
        renderer: &mut dyn Renderer,
        input: &FrameInput,
        tick: Tick,
        what: &'static str,
    ) -> Result<FrameOutput, StageError> {
        self.evaluator.evaluate(&input.channels, &mut self.weights, &mut self.binds);
        let uploads = self.upload.send_binds(renderer, &self.binds)?;
        self.plan.fill(&mut self.scene, self.rig.world(), &self.weights, self.root);
        self.submissions += 1;
        let index = if self.clock.is_live() { self.submissions } else { tick.index };
        self.scene.frame = FrameInfo { index, time: tick.time };
        let report = renderer.render(self.target, &self.scene, &self.quality).map_err(|source| StageError::render(what, source))?;
        Ok(FrameOutput { tick, report, uploads })
    }

    /// Makes the renderer quality of the following frames `quality`.
    pub fn set_quality(&mut self, quality: Quality) {
        self.quality = quality;
    }

    /// The quality frames render at.
    pub fn quality(&self) -> &Quality {
        &self.quality
    }

    /// Takes a resolved scene (camera, lights, background, post look) for the next frame, and puts the avatar where the
    /// scene's subject member stands. Returns the work owed beyond the copy, as
    /// [`Stage::set_scene`](crate::frame::Stage::set_scene) does.
    pub fn set_scene(&mut self, resolved: &ResolvedScene, subject_root: Mat4) -> Owed {
        let owed = self.link.set(resolved, &mut self.scene);
        self.set_subject_root(subject_root);
        owed
    }

    /// Moves the subject member alone, from the next frame.
    pub fn set_subject_root(&mut self, subject_root: Mat4) {
        if subject_root != self.subject_root {
            self.subject_root = subject_root;
            self.root = subject_root * self.input_root;
        }
    }

    /// What the last [`set_scene`](Self::set_scene) still owes the renderer.
    pub fn owed(&self) -> Owed {
        self.link.owed()
    }

    /// Records that the renderer did the work in `owed`.
    pub fn commit_scene(&mut self, owed: &Owed) {
        self.link.commit(owed);
    }

    /// The scene as last rendered.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The scene as the next frame will start from: set the camera, lights and post settings here.
    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
    }

    /// The target frames render into.
    pub fn target(&self) -> TargetId {
        self.target
    }

    /// The avatar the stage runs.
    pub fn avatar(&self) -> &ImportedAvatar {
        &self.avatar
    }

    /// The avatar's resources on the renderer.
    pub fn upload(&self) -> &AvatarUpload {
        &self.upload
    }

    /// The model-space matrix of `bone` as the last frame posed it (under the avatar's root), or `None` for a bone the
    /// skeleton lacks.
    pub fn bone_world(&self, bone: BoneId) -> Option<Mat4> {
        (bone.index() < self.avatar.skeleton.len()).then(|| self.rig.world().local[bone.index()].to_mat4())
    }

    /// The morph weights of the last frame, mesh after mesh, with each expression's applied weight.
    pub fn weights(&self) -> &MorphWeights {
        &self.weights
    }

    /// Gives every resource back to `renderer`.
    ///
    /// # Errors
    /// [`StageError::Render`] with the first release the renderer refused.
    pub fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        self.upload.release(renderer)
    }
}

impl std::fmt::Debug for AvatarStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvatarStage").field("meshes", &self.avatar.meshes.len()).finish_non_exhaustive()
    }
}
