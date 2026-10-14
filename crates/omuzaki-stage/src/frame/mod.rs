//! The frame loop: pose to rig to skin to part runtimes to dynamic meshes to scene, and the face update that
//! rides along (spec v3 §8 order).
//!
//! [`Stage::tick`] runs one frame in that order:
//!
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

use omuzaki_anim_api::{ChannelBuf, HumanoidPose, IkGoals, Tick};
use omuzaki_char_api::{BuiltCharacter, StepCtx};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_render_api::{FrameInfo, FrameReport, Quality, Renderer, Scene, TargetId};

use self::face::FaceRig;
use self::rig::{RigFrame, SkinPose};
use self::runtimes::DynamicMeshes;
use self::scene::ScenePlan;
use crate::error::StageError;
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

    /// A clock that advances by the `dt` each frame brings ([`FrameInput::dt`]): live use.
    pub fn live() -> Self {
        Self { fps: None, index: 0, time: 0.0 }
    }

    /// The next frame's tick. A fixed clock ignores `measured_dt`.
    fn next(&mut self, measured_dt: f32) -> Tick {
        let tick = match self.fps {
            Some(fps) => Tick::at_rate(self.index, fps),
            None => Tick { index: self.index, time: omuzaki_core::time::Seconds(self.time), dt: measured_dt },
        };
        self.index += 1;
        self.time += f64::from(tick.dt);
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
    /// The channels for the face.
    pub channels: ChannelBuf,
    /// Wind velocity in world space, metres per second.
    pub wind: Vec3,
    /// The character's world matrix.
    pub root: Mat4,
    /// Measured frame time in seconds. Only a [`StageClock::live`] clock reads it.
    pub dt: f32,
}

impl Default for FrameInput {
    fn default() -> Self {
        Self {
            pose: HumanoidPose::REST,
            goals: IkGoals::NONE,
            channels: ChannelBuf::default(),
            wind: Vec3::ZERO,
            root: Mat4::IDENTITY,
            dt: 0.0,
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

/// A character on a renderer with everything its frame loop needs.
pub struct Stage {
    built: BuiltCharacter,
    character: StageCharacter,
    rig: RigFrame,
    dynamic: DynamicMeshes,
    plan: ScenePlan,
    face: Option<Box<FaceRig>>,
    /// The character's world matrix of the last frame: the face system's and [`settle`](Stage::settle)'s.
    root: Mat4,
    scene: Scene,
    target: TargetId,
    quality: Quality,
    clock: StageClock,
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
        match Self::prepare(&built, &character) {
            Ok((rig, dynamic, plan)) => Ok(Self {
                built,
                character,
                rig,
                dynamic,
                plan,
                face: None,
                root: Mat4::IDENTITY,
                scene,
                target,
                quality,
                clock,
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
        Ok((RigFrame::new(&built.rig)?, DynamicMeshes::new(built, character)?, ScenePlan::new(built, character)?))
    }

    /// Runs one frame and renders it into the stage's target.
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses a vertex update or the frame, or whatever the face update reports (a layer that fails to paint, a staged variant whose vertex refresh the renderer refuses).
    /// The simulations have already stepped; the next call carries on from there.
    pub fn tick(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<FrameOutput, StageError> {
        let tick = self.clock.next(input.dt);
        self.root = input.root;
        self.rig.solve(tick, &input.pose, &input.goals);
        let pose = SkinPose { character: &self.built.rig, skin: self.rig.skin(), root: input.root };
        let ctx = StepCtx { dt: tick.dt, time: tick.time, pose: &pose, wind: input.wind };
        DynamicMeshes::step(&mut self.built.runtimes, &ctx);
        let uploads = self.dynamic.upload(&self.built.runtimes, renderer)?;
        self.update_face(renderer, tick, input)?;
        self.plan.fill(&mut self.scene, self.rig.skin(), input.root);
        self.scene.frame = FrameInfo { index: tick.index, time: tick.time };
        let report = renderer
            .render(self.target, &self.scene, &self.quality)
            .map_err(|source| StageError::render("the frame", source))?;
        Ok(FrameOutput { tick, report, uploads })
    }

    /// The scene as the next frame will start from: set the camera, lights and post settings here.
    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
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
        self.character.release(renderer)
    }
}
