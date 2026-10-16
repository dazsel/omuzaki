//! [`LiveStage`]: the kit character performing live (spec v3 §8, §10): a chosen source drives the performer, the
//! performer drives the stage, the stage renders, and the frame-budget scheduler reads every submission's cost.
//!
//! One thread runs it, once per display frame, through [`LiveStage::frame`]:
//!
//! 1. a frame source the input posted ([`SourceSlot`]) is swapped in, so a new camera or a stopped preview takes
//!    effect between steps, a scene posted to the [`Mailbox`] is taken in ([`Stage::set_scene`]), and a face variant
//!    switch is serviced ([`Stage::service_variants`]);
//! 2. the performer advances to `now` and, when a step ran, the stage ticks with its pose and channels and renders
//!    ([`PerformFrame`]); while [paused](LiveStage::set_paused) the stage redraws its last frame instead, so the output
//!    keeps flowing;
//! 3. the renderer's report goes to the [`Scheduler`], and the quality it decides on ([`Stage::set_quality`]) applies
//!    from the next frame.
//!
//! The performer's handle ([`PerformHandle`]) and the input ([`LiveInput`](super::input::LiveInput)) are the ops'
//! side; the frame thread never waits on them. Nothing in `frame` allocates in steady state beyond what the renderer
//! and the part runtimes cost (`tests/live.rs` counts).

use std::sync::Arc;

use omuzaki_budget::{Change, Scheduler};
use omuzaki_core::glam::Mat4;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{Quality, Renderer};

use super::frame::PerformFrame;
use super::host::{PerformControl, PerformError, PerformHandle, PerformHost, perform_channel};
use super::input::SourceSlot;
use super::Performer;
use crate::error::StageError;
use crate::frame::{FrameInput, FrameOutput, Stage};
use crate::scene::apply::{Mailbox, Owed};
use crate::scene::resolve::ResolvedScene;

/// What one live frame produced.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LiveFrame {
    /// The frame the stage rendered.
    pub output: FrameOutput,
    /// The change of quality the scheduler decided on this frame's report, for the app to show the person (§8). It
    /// applies from the next frame.
    pub change: Option<Change>,
}

/// The live loop's state: the performer and its control, the stage, the input's source slot and the scheduler.
pub struct LiveStage {
    performer: Performer,
    control: PerformControl,
    handle: PerformHandle,
    stage: Stage,
    frames: PerformFrame,
    sources: Arc<SourceSlot>,
    scheduler: Scheduler,
    paused: bool,
    /// Scenes posted from other threads; the frame thread takes the newest once per frame.
    scenes: Arc<Mailbox>,
    /// The scene last taken, kept to apply again when only the subject's root moves.
    scene: ResolvedScene,
    /// Where the scene puts the subject member ([`set_subject_root`](LiveStage::set_subject_root)).
    subject_root: Mat4,
    /// Set when the subject root changed and the stage has not seen it yet.
    subject_dirty: bool,
    /// The work the last scene owes the renderer (a ground disc, the skin floor), until the app does it and calls
    /// [`Stage::commit_scene`].
    owed: Owed,
}

impl LiveStage {
    /// The loop over `performer` and `stage`, reading sources from `sources` and keeping the frame within `budget`
    /// (for example [`budget::LIVE_GPU`](omuzaki_render_api::budget::LIVE_GPU)) at no more than `quality`. Make the
    /// stage with [`StageClock::live`](crate::frame::StageClock::live), so its time is the performer's.
    ///
    /// The character's expressions go to the performer's triggers at once ([`install_expressions`](Self::install_expressions)).
    /// Cold: allocates.
    ///
    /// # Errors
    /// What [`install_expressions`](Self::install_expressions) reports.
    pub fn new(
        performer: Performer,
        stage: Stage,
        sources: Arc<SourceSlot>,
        quality: Quality,
        budget: Seconds,
    ) -> Result<Self, PerformError> {
        let (handle, control) = perform_channel(&performer);
        let live = Self {
            performer,
            control,
            handle,
            stage,
            frames: PerformFrame::new(FrameInput::default()),
            sources,
            scheduler: Scheduler::new(quality, budget),
            paused: false,
            scenes: Arc::new(Mailbox::new()),
            scene: ResolvedScene::new(),
            subject_root: Mat4::IDENTITY,
            subject_dirty: false,
            owed: Owed::default(),
        };
        live.install_expressions()?;
        Ok(live)
    }

    /// Hands the stage character's expressions, with the performer's rest face, to the triggers, so `trigger.bind`
    /// accepts them. [`new`](Self::new) calls it; call it again after the face profile changes. A character without a
    /// face installs an empty set.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] when the performer has too many requests waiting; nothing changes then.
    pub fn install_expressions(&self) -> Result<(), PerformError> {
        let expressions = self.stage.built().face.as_ref().map(|face| face.profile.expressions.clone()).unwrap_or_default();
        self.handle.set_expressions(expressions, *self.performer.rest())
    }

    /// Runs one display frame at `now` (seconds on the loop's clock). Returns the frame, or `None` when no performer
    /// step was due and nothing was rendered.
    ///
    /// # Errors
    /// What [`Stage::service_variants`], [`Stage::tick`] or [`Stage::redraw`] reports. The performer has stepped; the next call carries on.
    pub fn frame(&mut self, renderer: &mut dyn Renderer, now: Seconds) -> Result<Option<LiveFrame>, StageError> {
        self.sources.swap_into(&mut self.performer);
        self.take_scene();
        // Variant switches are staged outside the tick (`frame::face`), against the input the last frame showed.
        self.stage.service_variants(renderer, self.frames.input())?;
        let output = if self.paused {
            self.stage.redraw(renderer, self.frames.input())?
        } else {
            match self.frames.run(&mut self.control, &mut self.performer, &mut self.stage, renderer, now)? {
                Some(output) => output,
                None => return Ok(None),
            }
        };
        let change = self.scheduler.observe(&output.report);
        if change.is_some() {
            self.stage.set_quality(self.scheduler.quality());
        }
        Ok(Some(LiveFrame { output, change }))
    }

    /// Takes a posted scene, or a moved subject root, into the stage. Never waits: a poster holding the mailbox means
    /// the next frame tries again. Allocation-free.
    fn take_scene(&mut self) {
        let fresh = self.scenes.take_into(&mut self.scene);
        if fresh || self.subject_dirty {
            self.subject_dirty = false;
            let owed = self.stage.set_scene(&self.scene, self.subject_root);
            // The stage compares against what the app committed, so the newest answer is the whole of what is owed.
            self.owed = owed;
        }
    }

    /// The mailbox to post resolved scenes to, from any thread: the camera, lights, background and post look the next
    /// frame starts from (newest wins).
    pub fn scenes(&self) -> Arc<Mailbox> {
        Arc::clone(&self.scenes)
    }

    /// Sets where the scene puts the subject member; the character's root of each frame goes below it. Takes effect on
    /// the next frame.
    pub fn set_subject_root(&mut self, root: Mat4) {
        if root != self.subject_root {
            self.subject_root = root;
            self.subject_dirty = true;
        }
    }

    /// The work the last scene owes the renderer, beyond the copy the stage made: a ground disc, the skin floor. Empty
    /// for a value-only change. The app does the work, then calls [`Stage::commit_scene`] through
    /// [`stage_mut`](Self::stage_mut); until then the same work stays owed.
    pub fn owed(&self) -> Owed {
        self.owed
    }

    /// Holds the performance (`paused`): every frame redraws the last one, with no step, so the output keeps
    /// rendering a still. Resuming carries on from the clock's `now`, skipping the paused time.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Starts the performance over: the performer and its timeline go back to tick 0 (a recorded track that loops,
    /// a seek). The stage keeps running, so its frames keep their order.
    pub fn restart(&mut self) {
        self.control.reset(&mut self.performer);
        self.frames.restart();
    }

    /// The input the last frame was ticked with: the performer's pose and channels, and the caller's root, wind and
    /// goals.
    pub fn input(&self) -> &FrameInput {
        self.frames.input()
    }

    /// The input of the next frame, to set the character's root, the wind or the goals.
    pub fn input_mut(&mut self) -> &mut FrameInput {
        self.frames.input_mut()
    }

    /// The performer's handle, for the ops ([`PerformHost`]).
    pub fn handle(&self) -> &PerformHandle {
        &self.handle
    }

    /// The stage.
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// The stage, to change its scene or face between frames.
    pub fn stage_mut(&mut self) -> &mut Stage {
        &mut self.stage
    }

    /// The frame-budget scheduler.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Takes the stage back, to release it from the renderer.
    pub fn into_stage(self) -> Stage {
        self.stage
    }
}

impl std::fmt::Debug for LiveStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveStage").field("paused", &self.paused).finish_non_exhaustive()
    }
}
