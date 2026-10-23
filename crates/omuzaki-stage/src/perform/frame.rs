//! The perform loop into the frame loop (spec v3 §8 frame order): each step's pose and channels become the input of
//! the stage's next frame.
//!
//! [`PerformFrame::run`] advances the [`PerformControl`] to `now` and, when a step ran, ticks the stage ([`FrameLoop`]) with that
//! step's pose (the rig reads it) and bus (the face reads it), then renders. When no step is due it renders nothing,
//! so a render loop faster than the step rate never shows the same pose twice and the stage's simulations advance in
//! step with the performer's: in simulation time, every run of the same trace gives the same frames.

use omuzaki_anim_api::Tick;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::Renderer;

use super::host::PerformControl;
use super::{Performer, STEP_HZ};
use crate::error::StageError;
use crate::frame::{FrameInput, FrameOutput};
use crate::stages::FrameLoop;

/// The input the stage's frames are built from, carried between frames: what the performer does not drive (the root,
/// the wind, the goals) stays as the caller set it.
#[derive(Clone, Debug)]
pub struct PerformFrame {
    input: FrameInput,
    /// The step the last frame showed, so the next frame's `dt` covers every step since (a slow loop runs several).
    last: Option<Tick>,
}

impl PerformFrame {
    /// Frames built from `input`: its pose and channels are replaced by the performer's each frame, the rest is kept.
    pub fn new(input: FrameInput) -> Self {
        Self { input, last: None }
    }

    /// The input of the next frame, to set the character's root, the wind or the goals.
    pub fn input_mut(&mut self) -> &mut FrameInput {
        &mut self.input
    }

    /// The input the last frame was ticked with.
    pub fn input(&self) -> &FrameInput {
        &self.input
    }

    /// Forgets the last step, so the next frame's `dt` is one step again: after a [`PerformControl::reset`].
    pub fn restart(&mut self) {
        self.last = None;
    }

    /// Advances the performer to `now` and, when a step ran, ticks `stage` with its pose and channels and renders.
    /// Returns the frame, or `None` when no step was due (nothing was rendered).
    ///
    /// The frame's `dt` is the time from the last frame's step to this one's, so a stage made with
    /// [`StageClock::live`](crate::frame::StageClock::live) keeps the performer's time exactly; a fixed clock steps by
    /// its own rate. Allocates nothing beyond what the performer and the stage cost.
    ///
    /// # Errors
    /// What the stage's [`tick`](FrameLoop::tick) reports. The performer has stepped; the next call carries on.
    pub fn run<S: FrameLoop>(
        &mut self,
        control: &mut PerformControl,
        performer: &mut Performer,
        stage: &mut S,
        renderer: &mut dyn Renderer,
        now: Seconds,
    ) -> Result<Option<FrameOutput>, StageError> {
        if !self.advance(control, performer, now) {
            return Ok(None);
        }
        stage.tick(renderer, &self.input).map(Some)
    }

    /// Advances the performer to `now` and, when a step ran, takes its pose and channels into the next frame's input
    /// without ticking a stage: what a cast member beside the subject does until the stage draws every member.
    /// Returns whether a step ran. Allocates nothing beyond what the performer costs.
    pub fn advance(&mut self, control: &mut PerformControl, performer: &mut Performer, now: Seconds) -> bool {
        let Some(out) = control.advance(performer, now) else { return false };
        self.input.pose = *out.pose;
        self.input.channels = out.bus.values;
        self.input.dt = step_dt(self.last, out.tick);
        // The stage shows the performer's own time: a frame that caught up several steps, or the first one after a
        // restart, is at that step's time, not at the sum of the frame times before it.
        self.input.time = Some(out.tick.time);
        self.last = Some(out.tick);
        true
    }
}

/// The time from step `last` to `now`: one step's time before the first.
#[allow(clippy::cast_possible_truncation)] // a gap of a few steps fits f32
fn step_dt(last: Option<Tick>, now: Tick) -> f32 {
    last.map_or(1.0 / STEP_HZ, |last| (now.time.0 - last.time.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_dt_covers_every_step_since_the_last_frame() {
        let tick = |i| Tick::at_rate(i, STEP_HZ);
        assert!((step_dt(None, tick(5)) - 1.0 / STEP_HZ).abs() < 1e-7);
        assert!((step_dt(Some(tick(5)), tick(6)) - 1.0 / STEP_HZ).abs() < 1e-6);
        assert!((step_dt(Some(tick(5)), tick(9)) - 4.0 / STEP_HZ).abs() < 1e-6);
    }
}
