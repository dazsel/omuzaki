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
pub mod ease;
mod gaze;
pub mod upload;

use omuzaki_anim_api::{BoneId, HumanoidBone, HumanoidPose, IkGoals, Tick};
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
use crate::frame::face::SETTLE_DT;
use crate::frame::rig::RigFrame;
use crate::frame::smear::Trails;
use crate::frame::squash::Jelly;
use crate::frame::timing::Holds;
use crate::frame::{FrameInput, FrameOutput, Prepared, StageClock};
use crate::scene::apply::{Owed, SceneLink, reserve};
use crate::scene::backdrop::StageBackdrop;
use crate::scene::environment::StageEnvironment;
use crate::props::staged::StageProps;
use crate::scene::ground::StageGround;
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
    ground: StageGround,
    environment: StageEnvironment,
    backdrop: StageBackdrop,
    props: StageProps,
    submissions: u64,
    tick: Tick,
    scene: Scene,
    target: TargetId,
    quality: Quality,
    clock: StageClock,
    /// The scene's animated timing (`scene.timing`): which frames move the body, the springs and the expressions.
    holds: Holds,
    /// The scene's squash and stretch of the drawn body (`scene.squash`).
    squash: Jelly,
    /// The body's recent palettes, for the scene's smear frames (`scene.smears`).
    trails: Trails,
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
            .map(|rig| rig.guarding(&avatar))
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
            pose: ease::at_ease(&avatar.bind),
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
            ground: StageGround::default(),
            environment: StageEnvironment::default(),
            backdrop: StageBackdrop::default(),
            props: StageProps::default(),
            submissions: 0,
            tick: Tick { index: 0, time: Seconds(0.0), dt: 0.0 },
            scene,
            target,
            quality,
            clock,
            holds: Holds::default(),
            squash: Jelly::default(),
            trails: Trails::default(),
        })
    }

    /// The pose a performer or animator layers onto: the avatar's bind (decision 0014 §4, as the kit's
    /// `seed_base_pose`) with T-pose arms lowered to hang at ease ([`ease::at_ease`]), so with nothing animating them
    /// a model authored in the T-pose doesn't stand in it.
    pub fn base_pose(&self) -> HumanoidPose {
        ease::at_ease(&self.avatar.bind)
    }

    /// Runs the surface fx `fx` over every material of the avatar whose name contains none of `spare`, or takes it off
    /// with `None`, as [`Stage::set_surface_fx`](crate::frame::Stage::set_surface_fx) does for a kit character. Cold.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a material; the materials are as they were.
    pub fn set_surface_fx(
        &mut self,
        renderer: &mut dyn Renderer,
        fx: Option<omuzaki_render_api::SurfaceFx>,
        spare: &[String],
    ) -> Result<(), StageError> {
        self.upload.set_surface_fx(renderer, &self.avatar, fx, spare)
    }

    /// Runs one frame and renders it into the stage's target. Returns the frame, with
    /// [`uploads`](FrameOutput::uploads) counting the materials an expression bind re-sent.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a material update or the frame. The rig has already stepped; the
    /// next call carries on from there.
    pub fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        let prepared = self.prepare_tick(renderer, input)?;
        self.submit(renderer, prepared, "the frame")
    }

    /// Everything [`tick`](Self::tick) does before the render.
    pub(crate) fn prepare_tick(
        &mut self,
        renderer: &mut dyn Renderer,
        input: &FrameInput,
    ) -> Result<Prepared, StageError> {
        let tick = self.clock.next(input.dt, input.time);
        self.tick = tick;
        let hold = self.holds.frame(tick);
        if hold.body {
            // A held pose holds where it stands too: the avatar's root moves with its pose.
            self.input_root = input.root;
            self.pose = input.pose;
            if let Some(gaze) = &self.gaze {
                gaze.apply(&input.channels, &mut self.pose);
            }
            self.goals = input.goals;
            self.rig.set_reaches(&input.reach);
        }
        self.root = self.subject_root * self.input_root;
        // The spring bones solve with the pose: held with it (catching up in one step at each new pose), or, when the
        // secondary motion flows, every frame under the held pose.
        if let Some(dt) = hold.secondary_dt {
            self.rig.solve(Tick { dt, ..tick }, &self.pose, &self.goals);
        }
        if hold.body {
            let hips = avatar_bone_world(&self.avatar, &self.rig, self.root, HumanoidBone::Hips);
            self.squash.step(tick.time.0, hips.map_or(f32::NAN, |m| m.w_axis.y));
        }
        self.prepare_draw(renderer, input, tick, hold.face, true)
    }

    /// Everything [`redraw`](Self::redraw) does before the render.
    pub(crate) fn prepare_redraw(
        &mut self,
        renderer: &mut dyn Renderer,
        input: &FrameInput,
    ) -> Result<Prepared, StageError> {
        self.prepare_draw(renderer, input, self.tick, true, false)
    }

    /// Renders the pose the last [`tick`](Self::tick) left with `input`'s channels, for a still that differs from the
    /// last frame only by its camera or expression: the clock and the rig do not move.
    ///
    /// # Errors
    /// As [`tick`](Self::tick).
    pub fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        let prepared = self.prepare_draw(renderer, input, self.tick, true, false)?;
        self.submit(renderer, prepared, "the still")
    }

    /// Steps the spring bones `steps` times at [`SETTLE_DT`] under the last frame's pose, goals and root, as the kit
    /// stage's [`settle`](crate::frame::Stage::settle) does: the clock doesn't move and nothing is rendered.
    pub fn settle(&mut self, steps: u32) {
        for step in 0..steps {
            let time = Seconds(f64::from(step) * f64::from(SETTLE_DT));
            self.rig.solve(Tick { index: u64::from(step), time, dt: SETTLE_DT }, &self.pose, &self.goals);
        }
    }

    /// [`settle`](Self::settle) from a clean start: the spring bones are reset to rest under the last frame's pose
    /// first, so a still depends on that pose and `steps` alone, never on the frames before it.
    pub fn settle_from_rest(&mut self, steps: u32) {
        self.rig.reset();
        self.rig.solve(Tick { dt: 0.0, ..self.tick }, &self.pose, &self.goals);
        self.settle(steps);
    }

    /// Step 4: expressions (when `face` moves this frame; a held face keeps the last weights) and the scene, ready to
    /// render, smeared along the trail; `record` keeps the frame for later frames' smears (a tick, not a redraw).
    fn prepare_draw(
        &mut self,
        renderer: &mut dyn Renderer,
        input: &FrameInput,
        tick: Tick,
        face: bool,
        record: bool,
    ) -> Result<Prepared, StageError> {
        let uploads = if face {
            self.evaluator.evaluate(&input.channels, &mut self.weights, &mut self.binds);
            self.upload.send_binds(renderer, &self.binds)?
        } else {
            0
        };
        let root = self.squash.matrix(self.root.w_axis.truncate()) * self.root;
        self.plan.fill(&mut self.scene, self.rig.world(), &self.weights, root);
        let own = self.scene.joints.len();
        if let Some((past, carry)) = self.trails.trail(tick.time.0, root) {
            AvatarPlan::smear(&mut self.scene, past, carry, &self.trails);
        }
        if record {
            self.trails.push(tick.time.0, &self.scene.joints[..own], root);
        }
        self.ground.push(&mut self.scene);
        let (avatar, rig) = (&self.avatar, &self.rig);
        self.props.push(&mut self.scene, &|bone| avatar_bone_world(avatar, rig, root, bone));
        self.submissions += 1;
        let index = if self.clock.is_live() { self.submissions } else { tick.index };
        self.scene.frame = FrameInfo { index, time: tick.time };
        Ok(Prepared { tick, uploads })
    }

    /// Step 5: renders the scene a prepare left.
    pub(crate) fn submit(
        &mut self,
        renderer: &mut dyn Renderer,
        prepared: Prepared,
        what: &'static str,
    ) -> Result<FrameOutput, StageError> {
        let report = renderer
            .render(self.target, &self.scene, &self.quality)
            .map_err(|source| StageError::render(what, source))?;
        Ok(FrameOutput { tick: prepared.tick, report, uploads: prepared.uploads })
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
        self.scene.environment = self.environment.use_for(resolved.environment.as_ref());
        if let Some(picture) = self.backdrop.background_for(resolved.backdrop_image.as_ref()) {
            self.scene.background = picture;
        }
        self.props.take(&resolved.props);
        self.holds.set(resolved.timing);
        self.squash.set(resolved.squash);
        self.trails.set(resolved.smears);
        self.set_subject_root(subject_root);
        owed
    }

    /// The animated timing in force (`scene.timing`, from the last [`set_scene`](Self::set_scene)).
    pub fn timing(&self) -> omuzaki_stage_api::Timing {
        self.holds.timing()
    }

    /// The squash and stretch drawn now (`scene.squash`): the body's height scale, `1` as posed.
    pub fn squash_scale(&self) -> f32 {
        self.squash.scale()
    }

    /// Moves the subject member alone, from the next frame.
    pub fn set_subject_root(&mut self, subject_root: Mat4) {
        if subject_root != self.subject_root {
            self.subject_root = subject_root;
            self.root = subject_root * self.input_root;
        }
    }

    /// The body as a touch with another member reads it (#2415): where the last frame left it, in the world.
    pub(crate) fn touch_body(&self) -> crate::interact::MemberBody {
        self.rig.body(self.root)
    }

    /// The scene's transform of this member ([`set_subject_root`](Self::set_subject_root)).
    pub(crate) fn subject_root_now(&self) -> Mat4 {
        self.subject_root
    }

    /// Keeps the arms out of another member's `parts` (world space) from the next frame, this member standing at
    /// `root` then; an empty slice clears them. Allocation-free.
    pub(crate) fn set_partner_parts(&mut self, parts: &[omuzaki_rig::ik::guard::Obstacle], root: Mat4) {
        self.rig.set_partner(parts, root);
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

    /// Where humanoid `bone` was in the last frame, in the world: the avatar's root times its model-space matrix.
    /// `None` for a bone the skeleton doesn't map.
    pub fn humanoid_world(&self, bone: HumanoidBone) -> Option<Mat4> {
        avatar_bone_world(&self.avatar, &self.rig, self.root, bone)
    }

    /// The props of the scene attached to cast member `member`, under `bone`'s answers, as
    /// [`Stage`](crate::frame::Stage) does. Hot: allocation-free.
    pub(crate) fn push_member_props(
        &mut self,
        member: &omuzaki_stage_api::MemberId,
        bone: &dyn Fn(HumanoidBone) -> Option<Mat4>,
    ) {
        self.props.push_member(&mut self.scene, member, bone);
    }

    /// The morph weights of the last frame, mesh after mesh, with each expression's applied weight.
    pub fn weights(&self) -> &MorphWeights {
        &self.weights
    }

    /// Gives every resource back to `renderer`.
    ///
    /// # Errors
    /// [`StageError::Render`] with the first release the renderer refused.
    pub fn release(mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let ground = self.ground.release(renderer);
        let environment = self.environment.release(renderer);
        let backdrop = self.backdrop.release(renderer);
        let props = self.props.release(renderer);
        self.upload.release(renderer).and(ground).and(environment).and(backdrop).and(props)
    }

    /// Does the renderer work the last [`set_scene`](Self::set_scene) owes for the ground, the environment and the props and
    /// records it as done, as [`Stage::pay_scene`](crate::frame::Stage::pay_scene). An imported avatar's materials
    /// have no skin floor, so that part is never owed work here. Cold.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses the disc or the environment, and [`StageError::Scene`] for an
    /// HDRI whose pixels weren't loaded; that part stays owed.
    pub fn pay_scene(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let owed = self.link.owed();
        if let Some(ground) = owed.ground {
            self.ground.set(renderer, &ground)?;
            self.link.commit(&Owed { ground: Some(ground), ..Owed::default() });
        }
        if let Some(source) = owed.environment {
            let wanted = self.link.wanted();
            self.environment.set(renderer, source, wanted.hdri.as_ref())?;
            self.scene.environment = self.environment.use_for(wanted.environment.as_ref());
            self.link.commit(&Owed { environment: Some(source), ..Owed::default() });
        }
        if let Some(asset) = owed.image {
            let wanted = self.link.wanted();
            self.backdrop.set(renderer, asset, wanted.image.as_ref())?;
            if let Some(picture) = self.backdrop.background_for(wanted.backdrop_image.as_ref()) {
                self.scene.background = picture;
            }
            self.link.commit(&Owed { image: Some(asset), ..Owed::default() });
        }
        if let Some(floor) = owed.skin_floor {
            self.link.commit(&Owed { skin_floor: Some(floor), ..Owed::default() });
        }
        if let Some(generation) = owed.props {
            let builds = std::sync::Arc::clone(&self.link.wanted().props.builds);
            self.props.set(renderer, &builds, &mut self.scene)?;
            self.link.commit(&Owed { props: Some(generation), ..Owed::default() });
        }
        Ok(())
    }
}

impl std::fmt::Debug for AvatarStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvatarStage").field("meshes", &self.avatar.meshes.len()).finish_non_exhaustive()
    }
}

/// Where humanoid `bone` of an imported avatar is in the world: `root` times its posed model-space matrix.
fn avatar_bone_world(avatar: &ImportedAvatar, rig: &RigFrame, root: Mat4, bone: HumanoidBone) -> Option<Mat4> {
    let id = avatar.skeleton.humanoid_bone(bone)?;
    rig.world().local.get(id.index()).map(|local| root * local.to_mat4())
}
