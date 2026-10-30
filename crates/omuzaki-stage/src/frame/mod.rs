//! The frame loop: pose to rig to skin to part runtimes to dynamic meshes to scene, and the face update that
//! rides along (spec v3 §8 order).
//!
//! [`Stage::tick`] runs one frame in that order:
//!
//! 0. a face variant staged by [`Stage::service_variants`] is swapped in ([`face`]), so the rest of the frame runs on it;
//! 1. the [`StageClock`] gives the frame's [`Tick`] (fixed `dt` and time, so a replay is exact, decision 0009);
//! 2. the pose in the [`FrameInput`] goes through the character's composed rig (humanoid pose, IK, spring bones) and
//!    out as skinning matrices ([`rig`]);
//! 3. every part runtime steps with a pose source over those matrices ([`runtimes`]);
//! 4. dynamic meshes whose revision moved are re-uploaded with `update_vertices`;
//! 5. the face update runs ([`face`]);
//! 6. the [`Scene`] is refilled with instances in render order and the joint runs ([`scene`]), and rendered.
//!
//! After construction nothing in steps 1 to 6 allocates in stage-owned code; the renderer and the runtimes cost what
//! they cost. That includes the frame that swaps in a new face variant: variants are built, warmed and prepared off
//! the frame path by [`Stage::service_variants`], which the frame thread calls once per frame outside `tick`
//! ([`face`] has the stages).

pub mod face;
pub mod pose;
pub(crate) mod rig;
pub(crate) mod runtimes;
pub(crate) mod scene;

use omuzaki_anim_api::{ChannelBuf, HumanoidPose, IkGoals, Reaches, Tick};
use omuzaki_char_api::{BuiltCharacter, LookParams, StepCtx};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{FrameInfo, FrameReport, Quality, Renderer, Scene, TargetId};

use self::face::FaceRig;
use self::rig::{RigFrame, SkinPose};
use self::runtimes::DynamicMeshes;
use self::scene::ScenePlan;
use crate::effects::frame::{EffectsFrame, FrameCtx, palette_colours};
use crate::effects::log::EffectLimits;
use crate::effects::service::StageEffects;
use crate::error::StageError;
use crate::scene::apply::{Owed, SceneLink, reserve};
use crate::scene::backdrop::StageBackdrop;
use crate::scene::environment::StageEnvironment;
use crate::props::staged::StageProps;
use crate::scene::ground::StageGround;
use crate::scene::resolve::ResolvedScene;
use crate::upload::{StageCharacter, UploadOptions};

/// Where a frame's `dt` and time come from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StageClock {
    /// `Some(fps)` for a fixed-rate clock; `None` for the measured one.
    fps: Option<f32>,
    index: u64,
    time: f64,
}

impl StageClock {
    /// A clock that advances exactly `1 / fps` per frame: tests, replays and offline renders.
    pub fn fixed(fps: f32) -> Self {
        Self { fps: Some(fps), index: 0, time: 0.0 }
    }

    /// A clock that advances by the `dt` each frame brings ([`FrameInput::dt`]): live use. A stage on this clock numbers
    /// its frames by submission, not by tick: every render, a [`redraw`](Stage::redraw) included, gets an index one
    /// higher than the last, so `omuzaki-budget`'s scheduler can tell reports apart through a pause or a loop of the
    /// timeline (`FrameInfo::index`, #821). The timeline position goes in `FrameInfo::time`.
    pub fn live() -> Self {
        Self { fps: None, index: 0, time: 0.0 }
    }

    /// Whether this is the measured clock of [`live`](Self::live).
    pub(crate) fn is_live(&self) -> bool {
        self.fps.is_none()
    }

    /// The next frame's tick. A fixed clock ignores `measured_dt` and `time`; a live one takes the frame's time from
    /// `time` when the caller knows it (the performer's step time), else from the dts it has summed.
    pub(crate) fn next(&mut self, measured_dt: f32, time: Option<Seconds>) -> Tick {
        let tick = match self.fps {
            Some(fps) => Tick::at_rate(self.index, fps),
            None => Tick {
                index: self.index,
                time: time.unwrap_or(omuzaki_core::time::Seconds(self.time)),
                dt: measured_dt,
            },
        };
        self.index += 1;
        self.time = tick.time.0 + f64::from(tick.dt);
        tick
    }
}

/// What one frame is given.
#[derive(Clone, Debug)]
pub struct FrameInput {
    /// The pose to show, layered by the animator onto the base pose ([`pose::seed_base_pose`]).
    pub pose: HumanoidPose,
    /// IK and look-at goals; [`IkGoals::NONE`] turns the stage off.
    pub goals: IkGoals,
    /// Where the hands reach on the body (a clip's hand on the chest, #2414): the rig turns each into a hand goal on
    /// the posed body, unless `goals` sets that hand itself. [`Reaches::NONE`] for none.
    pub reach: Reaches,
    /// The channel values of the frame: the face paints from them, and the character's bone drivers read them, so a
    /// channel a driver names (a custom channel folding a wing) moves its bone, and the spring chains under that bone,
    /// this frame. The rig keeps the last values a [`Stage::tick`] gave it:
    /// [`Stage::redraw`], [`Stage::settle`] and [`Stage::unpin_time`] pose the driven bones with them.
    pub channels: ChannelBuf,
    /// Wind velocity in world space, metres per second.
    pub wind: Vec3,
    /// The character's world matrix.
    pub root: Mat4,
    /// Measured frame time in seconds. Only a [`StageClock::live`] clock reads it.
    pub dt: f32,
    /// The frame's time on the loop's own timeline, when the caller keeps one (the performer's step time): a
    /// [`StageClock::live`] clock then shows exactly that, however many steps the frame covers or whether the timeline
    /// restarted, instead of the sum of the `dt`s. Only a live clock reads it.
    pub time: Option<Seconds>,
}

impl Default for FrameInput {
    fn default() -> Self {
        Self {
            pose: HumanoidPose::REST,
            goals: IkGoals::NONE,
            reach: Reaches::NONE,
            channels: ChannelBuf::default(),
            wind: Vec3::ZERO,
            root: Mat4::IDENTITY,
            dt: 0.0,
            time: None,
        }
    }
}

/// What one frame produced.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FrameOutput {
    /// The frame's tick.
    pub tick: Tick,
    /// The renderer's report for the frame.
    pub report: FrameReport,
    /// How many dynamic meshes were re-uploaded this frame.
    pub uploads: u32,
}

/// A frame made ready to render: the stage's scene holds it and nothing has been drawn
/// ([`StageMember`](crate::cast::StageMember)).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Prepared {
    /// The frame's tick.
    pub(crate) tick: Tick,
    /// How many dynamic meshes (or materials) were re-sent for it.
    pub(crate) uploads: u32,
}

/// How far `built` is raised so it stands on its lowest point: boots' soles and heels reach below the template's floor
/// line (the bare sole), and a character stands on what it wears, as a modelled character does. 0 for a character
/// nothing of which reaches below the floor. The same number [`Landmarks`](crate::scene::landmarks::Landmarks) frames
/// with.
pub fn stand_lift(built: &BuiltCharacter) -> f32 {
    crate::scene::landmarks::lift_for(built.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().copied()))
}

/// [`stand_lift`] as the translation the frame's root ends with.
pub(crate) fn stand_matrix(built: &BuiltCharacter) -> Mat4 {
    Mat4::from_translation(Vec3::new(0.0, stand_lift(built), 0.0))
}

/// What [`Stage::pin_time`] took from the stage, to give back with [`Stage::unpin_time`]: the state of the last frame.
#[derive(Clone, Debug)]
pub struct TimePin {
    /// The pin that was in force, if a caller had already pinned the stage (pins nest).
    pinned: Option<Tick>,
    tick: Tick,
    root: Mat4,
    input_root: Mat4,
    pose: HumanoidPose,
    goals: IkGoals,
    /// The channels the bone drivers last read (boxed: 512 bytes).
    channels: Box<ChannelBuf>,
}

/// A character on a renderer with everything its frame loop needs.
pub struct Stage {
    built: BuiltCharacter,
    character: StageCharacter,
    rig: RigFrame,
    dynamic: DynamicMeshes,
    plan: ScenePlan,
    face: Option<Box<FaceRig>>,
    /// The character's world matrix of the last frame: the subject's root times the input's. The face system's and
    /// [`settle`](Stage::settle)'s.
    root: Mat4,
    /// The input's own root of the last frame, so a new subject root can recompute `root`.
    input_root: Mat4,
    /// The scene's transform of the subject member, applied above every frame's root ([`set_scene`](Stage::set_scene)).
    subject_root: Mat4,
    /// Raises the character onto its lowest point ([`stand_lift`]): soles and heels stand on the floor, not in it.
    stand: Mat4,
    /// What the scene has committed for the renderer and still owes it ([`commit_scene`](Stage::commit_scene)).
    link: SceneLink,
    /// The scene's ground disc on the renderer.
    ground: StageGround,
    /// The scene's environment on the renderer.
    environment: StageEnvironment,
    /// The scene's backdrop picture on the renderer.
    backdrop: StageBackdrop,
    /// The scene's props on the renderer, and where each is drawn.
    props: StageProps,
    /// How many times a staged variant replaced the character, so a caller can tell the face profile may have changed.
    revision: u64,
    /// Renders submitted so far; numbers the frames of a live clock.
    submissions: u64,
    /// The pose and goals of the last frame: what [`settle`](Stage::settle) solves the rig with.
    pose: HumanoidPose,
    goals: IkGoals,
    /// The last frame's tick. Before the first frame it is time zero, which [`redraw`](Stage::redraw) renders at.
    tick: Tick,
    scene: Scene,
    target: TargetId,
    quality: Quality,
    clock: StageClock,
    /// While set, every tick takes this one instead of the clock's ([`Stage::pin_time`]).
    pinned: Option<Tick>,
    /// Particle effects, once attached ([`Stage::attach_effects`]).
    effects: Option<Box<EffectsFrame>>,
}

impl Stage {
    /// Uploads `built` to `renderer` and prepares its frame loop. `scene` carries what the loop does not own (camera,
    /// lights, background, post settings); `target` is where frames render. Cold: allocates.
    ///
    /// # Errors
    /// Any [`StageCharacter::upload`] error, [`StageError::Rig`] for a rig that can't run, [`StageError::NotUploaded`]
    /// for a runtime mesh the upload doesn't have. On an error nothing is left on the renderer.
    pub fn new(
        renderer: &mut dyn Renderer,
        built: BuiltCharacter,
        options: UploadOptions,
        scene: Scene,
        target: TargetId,
        quality: Quality,
        clock: StageClock,
    ) -> Result<Self, StageError> {
        let character = StageCharacter::upload(renderer, &built, options)?;
        // Room for the most lights a scene may have, so a later `set_scene` never grows the list.
        let mut scene = scene;
        reserve(&mut scene);
        match Self::prepare(&built, &character) {
            Ok((rig, dynamic, plan)) => Ok(Self {
                input_root: Mat4::IDENTITY,
                subject_root: Mat4::IDENTITY,
                stand: stand_matrix(&built),
                link: SceneLink::new(),
                ground: StageGround::default(),
                environment: StageEnvironment::default(),
                backdrop: StageBackdrop::default(),
                props: StageProps::default(),
                revision: 0,
                submissions: 0,
                built,
                character,
                rig,
                dynamic,
                plan,
                face: None,
                root: Mat4::IDENTITY,
                pose: HumanoidPose::REST,
                goals: IkGoals::NONE,
                tick: Tick { index: 0, time: Seconds(0.0), dt: 0.0 },
                scene,
                target,
                quality,
                clock,
                pinned: None,
                effects: None,
            }),
            Err(error) => {
                if let Err(cleanup) = character.release(renderer) {
                    tracing::warn!(%cleanup, "releasing a character whose stage could not start failed too");
                }
                Err(error)
            }
        }
    }

    fn prepare(
        built: &BuiltCharacter,
        character: &StageCharacter,
    ) -> Result<(RigFrame, DynamicMeshes, ScenePlan), StageError> {
        Ok((RigFrame::for_character(built)?, DynamicMeshes::new(built, character)?, ScenePlan::new(built, character)?))
    }

    /// Runs one frame and renders it into the stage's target.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a vertex update or the frame, or whatever the face update reports
    /// (a layer that fails to paint). The simulations have already stepped and the scene already names the frame's
    /// character; the next call carries on from there. A
    /// staged variant whose vertex refresh the renderer refuses fails the call before anything steps: the variant is
    /// abandoned and the live character stays.
    pub fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        let prepared = self.prepare_tick(renderer, input)?;
        self.submit(renderer, prepared, "the frame")
    }

    /// Everything [`tick`](Self::tick) does before the render: the scene names this frame's character and nothing has
    /// been drawn. A cast adds its other members' instances to the scene before [`submit`](Self::submit).
    pub(crate) fn prepare_tick(
        &mut self,
        renderer: &mut dyn Renderer,
        input: &FrameInput,
    ) -> Result<Prepared, StageError> {
        // A swap replaces the character's mesh table (the old one is retired, not freed, so the two differ for as long
        // as this compares them).
        let table = self.built.meshes.as_ptr();
        self.swap_variant(renderer)?;
        if !std::ptr::eq(table, self.built.meshes.as_ptr()) {
            self.revision += 1;
            if let Some(effects) = self.effects.as_mut() {
                effects.set_palette(palette_colours(&self.built.palette));
            }
        }
        let tick = self.pinned.unwrap_or_else(|| self.clock.next(input.dt, input.time));
        self.input_root = input.root;
        self.root = self.subject_root * input.root * self.stand;
        self.pose = input.pose;
        self.goals = input.goals;
        self.tick = tick;
        self.rig.set_reaches(&input.reach);
        self.rig.solve_with_channels(tick, &input.pose, &input.goals, &input.channels);
        let pose = SkinPose { character: &self.built.rig, skin: self.rig.skin(), root: self.root };
        let ctx = StepCtx::new(tick.dt, tick.time, &pose, input.wind);
        DynamicMeshes::step(&mut self.built.runtimes, &ctx);
        let prepared = self
            .dynamic
            .upload(&self.built.runtimes, renderer)
            .and_then(|uploads| self.update_face(renderer, tick, input).map(|()| uploads));
        // Filled even when the frame can't be prepared: a variant swapped in at the top of this tick, and the scene
        // must not go on naming the character it retired (its meshes are released).
        self.plan.fill(&mut self.scene, self.rig.skin(), self.root);
        self.ground.push(&mut self.scene);
        self.push_props();
        if let Some(effects) = self.effects.as_mut() {
            let ctx = FrameCtx {
                t: tick.time,
                dt: tick.dt,
                root: self.root,
                world: self.rig.world(),
                rig: &self.built.rig,
                camera: self.scene.camera,
                density: self.quality.particle_density,
            };
            effects.step(renderer, &ctx, &mut self.scene.particles);
        }
        self.scene.frame = self.frame_info(tick);
        let uploads = prepared?;
        Ok(Prepared { tick, uploads })
    }

    /// Renders the scene a [`prepare_tick`](Self::prepare_tick) or [`prepare_redraw`](Self::prepare_redraw) left.
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

    /// Makes every [`tick`](Stage::tick) and [`redraw`](Stage::redraw) run at `tick` (its index and time, and the `dt`
    /// the runtimes step by) instead of the stage clock's, until [`unpin_time`](Stage::unpin_time): whatever reads the
    /// frame's time (the face painter, the scene's frame info that seeds grain and sample jitter) then sees the same time
    /// on every frame, whatever the clock is and however many frames ran before. The clock itself does not move. For
    /// renders that must not depend on history: a character sheet, a still.
    pub fn pin_time(&mut self, tick: Tick) -> TimePin {
        let pin = TimePin {
            pinned: self.pinned,
            tick: self.tick,
            root: self.root,
            input_root: self.input_root,
            pose: self.pose,
            goals: self.goals,
            channels: Box::new(*self.rig.channels()),
        };
        self.pinned = Some(tick);
        pin
    }

    /// Ends [`pin_time`](Stage::pin_time): puts back the pin that was in force before it (none, or an outer caller's),
    /// and the last frame's tick, root, pose, goals and bone-driver channels, with the rig, the scene's instances and
    /// its frame info following them, so the stage looks as it did before the pin (what the runtimes settled to stays).
    /// Nothing is sent to the renderer.
    pub fn unpin_time(&mut self, pin: &TimePin) {
        self.pinned = pin.pinned;
        (self.tick, self.root, self.input_root, self.pose, self.goals) =
            (pin.tick, pin.root, pin.input_root, pin.pose, pin.goals);
        // A solve for no time: the bones follow the restored pose and channels without stepping the springs.
        self.rig.solve_with_channels(Tick { dt: 0.0, ..pin.tick }, &pin.pose, &pin.goals, &pin.channels);
        self.plan.fill(&mut self.scene, self.rig.skin(), pin.root);
        self.ground.push(&mut self.scene);
        self.push_props();
        self.scene.frame = FrameInfo { index: pin.tick.index, time: pin.tick.time };
    }

    /// Where every vertex of the character is in the frame the stage last prepared, in world space: a dynamic mesh at
    /// the shape its runtime holds now (the vertices [`tick`](Stage::tick) and [`redraw`](Stage::redraw) send the
    /// renderer), a skinned mesh blended by the current joint palette, as the renderer does, and everything under the
    /// character's root. Morph targets are not applied. Cold: allocates.
    pub fn drawn_vertices(&self) -> Vec<Vec3> {
        let skin = self.rig.skin();
        let mut out = Vec::new();
        for built_mesh in &self.built.meshes {
            let mesh = &built_mesh.mesh;
            let positions = self.runtime_positions(built_mesh).unwrap_or(&mesh.geometry.positions);
            let bones = scene::joint_bones(&self.built, mesh);
            for (i, p) in positions.iter().enumerate() {
                let local = match (mesh.geometry.joints.get(i), mesh.geometry.weights.get(i)) {
                    (Some(joints), Some(weights)) => {
                        let blend = joints.iter().zip(weights).fold(Mat4::ZERO, |sum, (joint, weight)| {
                            let bone = bones.get(usize::from(*joint)).copied().flatten();
                            sum + scene::joint_matrix(bone, skin) * *weight
                        });
                        blend.transform_point3(*p)
                    }
                    _ => *p,
                };
                out.push(self.root.transform_point3(local));
            }
        }
        out
    }

    /// The vertices a runtime holds for `mesh` now, if it rewrites it.
    fn runtime_positions<'a>(&'a self, mesh: &omuzaki_char_api::BuiltMesh) -> Option<&'a [Vec3]> {
        if !mesh.mesh.dynamic {
            return None;
        }
        self.built.runtimes.iter().filter(|slot| slot.part == mesh.part).find_map(|slot| {
            (0..slot.runtime.dynamic_count())
                .filter_map(|index| slot.runtime.dynamic_mesh(index))
                .find(|view| view.mesh == mesh.local)
                .map(|view| view.positions)
        })
    }

    /// The frame info of a render at `tick`, and the submission it counts. A live clock numbers by submission, so every
    /// render is one higher than the last whatever the timeline does (pause, loop, restart); every other clock, and a
    /// pinned stage, number by the tick, which repeats on purpose for a render that must not depend on history.
    fn frame_info(&mut self, tick: Tick) -> FrameInfo {
        self.submissions += 1;
        let index = if self.clock.is_live() && self.pinned.is_none() { self.submissions } else { tick.index };
        FrameInfo { index, time: tick.time }
    }

    /// Attaches particle effects (particles.md D12): from the next frame the stage drains `service`'s requests, runs
    /// the scene's weather and fills the scene's particles under `limits` ([`EffectLimits::live`] while performing,
    /// [`EffectLimits::offline`] for renders). The app registers the same `service` as the ops' `EffectService`.
    /// Cold. Attaching again replaces the effects (release the old ones first with [`release`](Self::release)'s
    /// renderer, or they stay until the renderer drops).
    pub fn attach_effects(&mut self, service: std::sync::Arc<StageEffects>, limits: EffectLimits) {
        crate::effects::frame::reserve(&mut self.scene.particles);
        self.effects = Some(Box::new(EffectsFrame::new(service, limits, palette_colours(&self.built.palette))));
    }

    /// The particle effects, once attached.
    pub fn effects(&self) -> Option<&EffectsFrame> {
        self.effects.as_deref()
    }

    /// The particle effects, once attached, to switch scenes, change limits or restore a checkpoint.
    pub fn effects_mut(&mut self) -> Option<&mut EffectsFrame> {
        self.effects.as_deref_mut()
    }

    /// Makes the renderer quality of the following frames `quality`: what the frame-budget scheduler decided.
    pub fn set_quality(&mut self, quality: Quality) {
        self.quality = quality;
    }

    /// The quality frames render at.
    pub fn quality(&self) -> &Quality {
        &self.quality
    }

    /// Takes a resolved scene (camera, lights, background, post look) into the stage, for the next frame: copies it
    /// into the [`Scene`] without allocating ([`apply::Applier`](crate::scene::apply)), and puts the character where the
    /// scene's subject member stands, `subject_root`, above the root each frame's input brings. Returns the work the
    /// renderer is owed beyond the copy (a ground disc, the skin floor) and which stays owed until
    /// [`commit_scene`](Self::commit_scene) says it was done; a value-only change owes nothing.
    ///
    /// Allocation-free for a scene the lights of which fit ([`MAX_LIGHTS`](omuzaki_render_api::limits::MAX_LIGHTS)).
    pub fn set_scene(&mut self, resolved: &ResolvedScene, subject_root: Mat4) -> Owed {
        let owed = self.link.set(resolved, &mut self.scene);
        if let Some(effects) = self.effects.as_mut() {
            effects.set_scene(&resolved.effects);
        }
        self.scene.environment = self.environment.use_for(resolved.environment.as_ref());
        if let Some(picture) = self.backdrop.background_for(resolved.backdrop_image.as_ref()) {
            self.scene.background = picture;
        }
        self.props.take(&resolved.props);
        self.set_subject_root(subject_root);
        owed
    }

    /// Moves the subject member alone: the scene's camera, lights and look stay as they are. Takes effect from the next
    /// frame, or the next [`redraw`](Self::redraw) of a paused stage.
    pub fn set_subject_root(&mut self, subject_root: Mat4) {
        if subject_root != self.subject_root {
            self.subject_root = subject_root;
            // A paused stage redraws from `root`, so it moves with the subject at once.
            self.root = subject_root * self.input_root * self.stand;
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

    /// What the last [`set_scene`](Self::set_scene) still owes the renderer: nothing once everything was committed.
    pub fn owed(&self) -> Owed {
        self.link.owed()
    }

    /// Records that the renderer did the work in `owed` (from some earlier [`set_scene`](Self::set_scene)), and works
    /// out again what the current scene still owes against what the renderer now has. Work for a scene that has since
    /// been replaced is recorded as done all the same, because the renderer did it: if scene A added a ground, scene B
    /// removed it and A's work finishes late, the debt becomes the removal.
    pub fn commit_scene(&mut self, owed: &Owed) {
        self.link.commit(owed);
    }

    /// How many times a variant swap replaced the character: changes when its face profile (and so its expressions) may
    /// have.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The scene as the next frame will start from: set the camera, lights and post settings here.
    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
    }

    /// Renders the state the last [`tick`](Stage::tick) left, with `input`'s face channels and the camera and lights of
    /// the scene, for a still that differs from the last frame only by its camera or expression.
    ///
    /// Nothing advances: the clock, the rig and the part runtimes stay where the last tick left them, so the same shot
    /// renders the same pixels however many shots came before it. The face is repainted for `input.channels`, but the
    /// bone drivers keep the last tick's channels, as the rest of the rig keeps its pose; the pose and goals of `input`
    /// and its `dt`, `wind` and `root` are not read, so a change of pose or of a driven bone needs a
    /// [`tick`](Stage::tick). A variant switch that is pending completes on a tick, not here. It sends the renderer the
    /// dynamic meshes whose revision moved since the last upload, as a tick does, and reports how many in
    /// [`FrameOutput::uploads`]: after [`settle`](Stage::settle) the still shows the settled hair. It allocates nothing
    /// in steady state.
    ///
    /// # Errors
    /// As [`tick`](Stage::tick) for the face and the render: [`StageError::Render`] if the renderer refuses the frame,
    /// or whatever the face update reports.
    pub fn redraw(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        let prepared = self.prepare_redraw(renderer, input)?;
        self.submit(renderer, prepared, "the still")
    }

    /// Everything [`redraw`](Self::redraw) does before the render.
    pub(crate) fn prepare_redraw(
        &mut self,
        renderer: &mut dyn Renderer,
        input: &FrameInput,
    ) -> Result<Prepared, StageError> {
        let uploads = self.dynamic.upload(&self.built.runtimes, renderer)?;
        // A pin set since the last tick holds here too: the face and the frame info see the pinned time.
        let tick = self.pinned.unwrap_or(self.tick);
        self.update_face(renderer, tick, input)?;
        self.plan.fill(&mut self.scene, self.rig.skin(), self.root);
        self.ground.push(&mut self.scene);
        self.push_props();
        // What the last tick wrote into the scene, so a redraw before any tick doesn't render the caller's frame info.
        self.scene.frame = self.frame_info(tick);
        Ok(Prepared { tick, uploads })
    }

    /// The scene as last rendered.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The target frames render into.
    pub fn target(&self) -> TargetId {
        self.target
    }

    /// The character the stage runs.
    pub fn built(&self) -> &BuiltCharacter {
        &self.built
    }

    /// The character's resources on the renderer.
    pub fn character(&self) -> &StageCharacter {
        &self.character
    }

    /// Where `bone` was in the last frame, in the world: the character's root times the bone's solved model-space
    /// transform. `None` for a bone the character lacks.
    pub fn bone_world(&self, bone: omuzaki_anim_api::HumanoidBone) -> Option<Mat4> {
        kit_bone_world(&self.built, &self.rig, self.root, bone)
    }

    /// The scene's props into the frame: the unattached ones where they stand, the ones on the character's bones
    /// where the frame posed them. Hot: allocation-free.
    fn push_props(&mut self) {
        let (built, rig, root) = (&self.built, &self.rig, self.root);
        self.props.push(&mut self.scene, &|bone| kit_bone_world(built, rig, root, bone));
    }

    /// The props of the scene attached to cast member `member`, under `bone`'s answers: what a cast adds once the
    /// member's frame is posed. Hot: allocation-free.
    pub(crate) fn push_member_props(
        &mut self,
        member: &omuzaki_stage_api::MemberId,
        bone: &dyn Fn(omuzaki_anim_api::HumanoidBone) -> Option<Mat4>,
    ) {
        self.props.push_member(&mut self.scene, member, bone);
    }

    /// The skinning matrices of the last frame, indexed by bone.
    pub fn skin(&self) -> &[Mat4] {
        self.rig.skin()
    }

    /// Gives every resource back to `renderer` (see [`StageCharacter::release`]).
    ///
    /// # Errors
    /// [`StageError::Render`] with the first release the renderer refused.
    pub fn release(mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        if let Some(rig) = self.face.as_mut() {
            // A staged variant or a retired character still holds renderer resources.
            face::discard_switch(renderer, rig);
        }
        let ground = self.ground.release(renderer);
        let environment = self.environment.release(renderer);
        let backdrop = self.backdrop.release(renderer);
        let props = self.props.release(renderer);
        let effects = match self.effects.as_mut() {
            Some(effects) => effects.release(renderer).map_err(|source| StageError::render("the effects", source)),
            None => Ok(()),
        };
        self.character.release(renderer).and(ground).and(environment).and(backdrop).and(props).and(effects)
    }

    /// Draws the character in `look` (LK-06, #714): the materials the look changes are updated on the renderer in
    /// place, with no mesh or texture created and nothing rebuilt, so a `look.*` edit shows on the next frame. Cold.
    /// Returns how many materials were updated.
    ///
    /// # Errors
    /// [`StageError::Rebuild`] for a look the materials can't take (nothing changes then); what
    /// [`StageCharacter`]'s update refuses, after which the previous look is put back on the character and the renderer.
    pub fn apply_look(&mut self, renderer: &mut dyn Renderer, look: &LookParams) -> Result<u32, StageError> {
        let before = self.built.look().clone();
        let changed = self
            .built
            .apply_look(look)
            .map_err(|e| StageError::Rebuild { reason: format!("the look can't be drawn: {e}") })?;
        self.character.update_materials(renderer, &self.built, &changed).inspect_err(|_| {
            // The previous look was drawable a moment ago, so this restores both sides.
            if let Ok(back) = self.built.apply_look(&before)
                && let Err(error) = self.character.update_materials(renderer, &self.built, &back)
            {
                tracing::warn!(%error, "restoring the previous look after a refused look edit failed too");
            }
        })
    }

    /// Puts `floor` on every toon skin material of the character (or each back to its own, for `None`), outside any
    /// scene: what a [`CastStage`](crate::cast::CastStage) does for a member, which has no scene of its own. Cold.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a material.
    pub(crate) fn set_skin_floor(&mut self, renderer: &mut dyn Renderer, floor: Option<f32>) -> Result<(), StageError> {
        self.character.set_skin_floor(renderer, &self.built, floor).map(drop)
    }

    /// Does the renderer work the last [`set_scene`](Self::set_scene) owes and records it as done: the ground (a new
    /// disc, or none), the skin floor (every toon skin material updated in place to the scene's floor, or back to
    /// its own), the environment (made from its sky or its loaded HDRI, or released) and the props (the builds the
    /// renderer doesn't have uploaded, the ones the scene dropped released). The floor stays on the materials through
    /// palette changes, variant swaps and look edits. Cold.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses the disc, a material or the environment, and
    /// [`StageError::Scene`] for an HDRI whose pixels weren't loaded; that part stays owed.
    pub fn pay_scene(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let owed = self.link.owed();
        if let Some(ground) = owed.ground {
            self.ground.set(renderer, &ground)?;
            self.link.commit(&Owed { ground: Some(ground), ..Owed::default() });
        }
        if let Some(floor) = owed.skin_floor {
            self.character.set_skin_floor(renderer, &self.built, floor)?;
            self.link.commit(&Owed { skin_floor: Some(floor), ..Owed::default() });
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
        if let Some(generation) = owed.props {
            let builds = std::sync::Arc::clone(&self.link.wanted().props.builds);
            self.props.set(renderer, &builds, &mut self.scene)?;
            self.link.commit(&Owed { props: Some(generation), ..Owed::default() });
        }
        Ok(())
    }
}

/// Where `bone` of a kit character is in the world: `root` times its solved model-space transform.
fn kit_bone_world(
    built: &BuiltCharacter,
    rig: &RigFrame,
    root: Mat4,
    bone: omuzaki_anim_api::HumanoidBone,
) -> Option<Mat4> {
    let id = built.rig.bone(&omuzaki_char_api::JointRef::Humanoid(bone))?;
    let local = rig.world().local.get(id.index())?;
    Some(root * Mat4::from_rotation_translation(local.rotation, local.translation))
}
