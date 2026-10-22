//! [`LiveScene`]: the render thread's end of the live slice. It takes the newest [`LiveFrame`] from the mailbox and
//! runs the stage's frame with it, which fills the renderer's `Scene` and renders it into the stage's target.
//!
//! The main viewport and the output window draw the same frame: the output window shows a copy of what this renders
//! (`output_window`), never a second render.

use omuzaki_anim_api::HumanoidPose;
use omuzaki_render_api::{Renderer, Scene};
use omuzaki_stage::StageError;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, FrameOutput, Stage};

use super::mailbox::{FrameReader, LiveFrame};

/// The stage driven by the perform thread's frames.
pub struct LiveScene {
    stage: Stage,
    reader: FrameReader,
    /// The input of the last frame; only its pose and channels change from frame to frame.
    input: FrameInput,
    last: Option<LiveFrame>,
}

impl LiveScene {
    /// The stage `stage`, fed from `reader`.
    pub fn new(stage: Stage, reader: FrameReader) -> Self {
        Self { stage, reader, input: FrameInput::default(), last: None }
    }

    /// The pose the performer should be built on for this character: its bind pose, as the stage seeds it. Pass it as
    /// the performer's `base`, so the poses it publishes are the character's own.
    pub fn base_pose(stage: &Stage) -> HumanoidPose {
        let mut base = HumanoidPose::REST;
        seed_base_pose(&stage.built().rig, &mut base);
        base
    }

    /// Renders the newest frame if one arrived since the last call, and returns what the stage reported; `None` when
    /// the perform thread has published nothing new (the last picture stays). `dt` is the measured time since the
    /// previous render, which only a live stage clock reads. Allocates nothing in steady state.
    ///
    /// # Errors
    /// Whatever [`Stage::tick`] returns.
    pub fn frame(&mut self, renderer: &mut dyn Renderer, dt: f32) -> Result<Option<FrameOutput>, StageError> {
        let Some(frame) = self.reader.take_new() else { return Ok(None) };
        self.input.pose = frame.pose;
        self.input.channels = frame.channels;
        self.input.dt = dt;
        self.last = Some(frame);
        self.stage.tick(renderer, &self.input).map(Some)
    }

    /// The perform frame the last render showed.
    pub fn last_frame(&self) -> Option<&LiveFrame> {
        self.last.as_ref()
    }

    /// The scene as last rendered.
    pub fn scene(&self) -> &Scene {
        self.stage.scene()
    }

    /// The stage, for its target and its scene settings.
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// The stage, to change the camera, the lights or the background between frames.
    pub fn stage_mut(&mut self) -> &mut Stage {
        &mut self.stage
    }

    /// Gives the stage's resources back to `renderer` (the first step of the quit order, `live::wiring`).
    ///
    /// # Errors
    /// The first release the renderer refused.
    pub fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        self.stage.release(renderer)
    }
}

impl std::fmt::Debug for LiveScene {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveScene").field("last", &self.last.map(|frame| frame.tick)).finish_non_exhaustive()
    }
}
