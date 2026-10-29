//! [`LiveScene`]: the render thread's end of the live slice. It takes the newest [`LiveFrame`] from the mailbox and
//! runs the scene's frame with it: the frame drives the scene's subject, every other cast member stands as it was, and
//! the whole scene is filled into the renderer's `Scene` and rendered into the stage's target.
//!
//! The main viewport and the output window draw the same frame: the output window shows a copy of what this renders
//! (`output_window`), never a second render.

use omuzaki_anim_api::HumanoidPose;
use omuzaki_render_api::{Renderer, Scene};
use omuzaki_stage::StageError;
use omuzaki_stage::frame::{FrameInput, FrameOutput};
use omuzaki_stage::scene::stage::SceneStage;
use omuzaki_stage::stages::FrameLoop;

use super::mailbox::{FrameReader, LiveFrame};

/// The scene driven by the perform thread's frames.
pub struct LiveScene {
    stage: SceneStage,
    reader: FrameReader,
    /// The input of the last frame; only its pose and channels change from frame to frame.
    input: FrameInput,
    last: Option<LiveFrame>,
    /// The scene changed since the last render: draw it again even without a new perform frame.
    stale: bool,
}

impl LiveScene {
    /// The scene `stage`, its subject fed from `reader`.
    pub fn new(stage: SceneStage, reader: FrameReader) -> Self {
        let input = FrameInput { pose: Self::base_pose(&stage), ..FrameInput::default() };
        Self { stage, reader, input, last: None, stale: false }
    }

    /// The pose the performer should be built on for the scene's subject: its bind pose, as the stage seeds it. Pass
    /// it as the performer's `base`, so the poses it publishes are the character's own.
    pub fn base_pose(stage: &SceneStage) -> HumanoidPose {
        stage.base_pose(stage.subject()).unwrap_or(HumanoidPose::REST)
    }

    /// Renders the newest frame if one arrived since the last call (or the scene changed), and returns what the stage
    /// reported; `None` when there is nothing new to draw (the last picture stays). `dt` is the measured time since
    /// the previous render, which only a live stage clock reads. Allocates nothing in steady state.
    ///
    /// # Errors
    /// Whatever the cast's tick returns.
    pub fn frame(&mut self, renderer: &mut dyn Renderer, dt: f32) -> Result<Option<FrameOutput>, StageError> {
        match self.reader.take_new() {
            Some(frame) => {
                self.input.pose = frame.pose;
                self.input.channels = frame.channels;
                self.last = Some(frame);
            }
            None if self.stale => {}
            None => return Ok(None),
        }
        self.stale = false;
        self.input.dt = dt;
        self.stage.tick(renderer, &self.input).map(Some)
    }

    /// Draws the next frame even if no perform frame arrives for it: the scene changed.
    pub fn redraw_next(&mut self) {
        self.stale = true;
    }

    /// The perform frame the last render showed.
    pub fn last_frame(&self) -> Option<&LiveFrame> {
        self.last.as_ref()
    }

    /// The scene as last rendered.
    pub fn scene(&self) -> &Scene {
        self.stage.scene()
    }

    /// The staged scene, for its target, its cast and its settings.
    pub fn stage(&self) -> &SceneStage {
        &self.stage
    }

    /// The staged scene, to follow the document or change the camera, the lights or the background between frames.
    pub fn stage_mut(&mut self) -> &mut SceneStage {
        &mut self.stage
    }

    /// Gives the scene's resources back to `renderer` (the first step of the quit order, `live::wiring`). The target
    /// it drew into stays on the renderer, which is dropped with it.
    ///
    /// # Errors
    /// The first release the renderer refused.
    pub fn release(self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        self.stage.release(renderer)
    }
}

impl std::fmt::Debug for LiveScene {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveScene")
            .field("scene", &self.stage.scene_id())
            .field("last", &self.last.map(|frame| frame.tick))
            .finish_non_exhaustive()
    }
}
