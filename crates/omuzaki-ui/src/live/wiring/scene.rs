//! [`LiveScene`]: the render thread's end of the live slice. It takes the newest [`LiveFrame`] from the mailbox and
//! runs the scene's frame with it: the frame drives the scene's subject, every other cast member stands as it was, and
//! the whole scene is filled into the renderer's `Scene` and rendered into the stage's target.
//!
//! The main viewport and the output window draw the same frame: the output window shows a copy of what this renders
//! (`output_window`), never a second render.
//!
//! **Adaptive quality** (#2593). With [`set_adaptive`](LiveScene::set_adaptive) on, every frame's report goes to the
//! frame-budget [`Scheduler`], which lowers the stage's quality one rung of the degrade ladder at a time when the
//! frame stays over the spec §10 budget (a game beside the show taking the GPU) and gives it back when there is room.
//! Held (a take recording), the quality stays as it is, so a recording keeps one look.

use omuzaki_anim_api::HumanoidPose;
use omuzaki_budget::{Change, Direction, Scheduler};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{Quality, Renderer, Scene, budget};
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
    /// Adaptive quality's scheduler, which keeps the quality the stage was made with as its request.
    scheduler: Scheduler,
    /// Whether the scheduler may change quality.
    adaptive: bool,
    /// Whether it is holding still while on (a take recording).
    held: bool,
    /// Changes of quality so far.
    changes: u64,
    /// The last change.
    last_change: Option<Change>,
}

/// Adaptive quality as it stands, for the health meter and `program.status`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct QualityState {
    /// Whether the scheduler may change quality.
    pub adaptive: bool,
    /// Holding still while on (a take recording).
    pub held: bool,
    /// Ladder rungs applied (0 is the quality asked for).
    pub rung: u32,
    /// Changes so far.
    pub changes: u64,
    /// The last change.
    pub last: Option<Change>,
    /// The smoothed frame time held against the budget.
    pub smoothed: Option<Seconds>,
    /// The budget.
    pub budget: Seconds,
}

impl QualityState {
    /// The last change as words: `"down: bloom_off"`.
    pub fn last_words(&self) -> Option<String> {
        self.last.map(|c| {
            let way = match c.direction {
                Direction::Down => "down",
                Direction::Up => "up",
            };
            format!("{way}: {}", c.step.name())
        })
    }
}

/// The spec §10 budget a quality is held to: the full look's when it is the high preset, the live budget otherwise.
fn budget_for(quality: &Quality) -> Seconds {
    if *quality == Quality::high() { budget::HIGH_GPU } else { budget::LIVE_GPU }
}

impl LiveScene {
    /// The scene `stage`, its subject fed from `reader`.
    pub fn new(stage: SceneStage, reader: FrameReader) -> Self {
        let input = FrameInput { pose: Self::base_pose(&stage), ..FrameInput::default() };
        let requested = *stage.quality();
        Self {
            stage,
            reader,
            input,
            last: None,
            stale: false,
            scheduler: Scheduler::new(requested, budget_for(&requested)),
            adaptive: false,
            held: false,
            changes: 0,
            last_change: None,
        }
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
        let out = self.stage.tick(renderer, &self.input)?;
        if self.adaptive
            && !self.held
            && let Some(change) = self.scheduler.observe(&out.report)
        {
            self.stage.set_quality(self.scheduler.quality());
            self.changes += 1;
            self.last_change = Some(change);
            tracing::info!(
                step = change.step.name(),
                direction = ?change.direction,
                measured_ms = change.measured.0 * 1000.0,
                budget_ms = change.budget.0 * 1000.0,
                "adaptive quality changed the stage's quality"
            );
        }
        Ok(Some(out))
    }

    /// Adaptive quality on or off, and whether it holds still while on (a take recording without leave to change the
    /// look). Turned off, the stage goes back to the quality it was made with.
    pub fn set_adaptive(&mut self, on: bool, held: bool) {
        if self.adaptive && !on {
            let requested = self.scheduler.requested();
            self.scheduler.set_requested(requested, budget_for(&requested));
            self.stage.set_quality(self.scheduler.quality());
            self.stale = true;
        }
        (self.adaptive, self.held) = (on, held);
    }

    /// Adaptive quality as it stands.
    pub fn quality_state(&self) -> QualityState {
        QualityState {
            adaptive: self.adaptive,
            held: self.held,
            rung: u32::try_from(self.scheduler.rungs() + self.scheduler.density_rungs()).unwrap_or(u32::MAX),
            changes: self.changes,
            last: self.last_change,
            smoothed: self.scheduler.smoothed(),
            budget: self.scheduler.budget(),
        }
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
