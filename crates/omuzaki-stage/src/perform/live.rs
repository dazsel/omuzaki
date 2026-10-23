//! [`LiveStage`]: the kit character performing live (spec v3 §8, §10): a chosen source drives the performer, the
//! performer drives the stage, the stage renders, and the frame-budget scheduler reads every submission's cost.
//!
//! One thread runs it, once per display frame, through [`LiveStage::frame`]:
//!
//! 1. a frame source the input posted ([`SourceSlot`]) is swapped in, so a new camera or a stopped preview takes
//!    effect between steps, and a scene posted to the [`Mailbox`] is taken in ([`Stage::set_scene`]);
//! 2. the performer advances to `now` and, when a step ran, the stage ticks with its pose and channels and renders
//!    ([`PerformFrame`]); while [paused](LiveStage::set_paused) the stage redraws its last frame instead, so the output
//!    keeps flowing;
//! 3. the renderer's report goes to the [`Scheduler`], and the quality it decides on ([`Stage::set_quality`]) applies
//!    from the next frame.
//!
//! The loop then calls [`LiveStage::service`] once, outside the frame's deadline: it moves a face variant switch along
//! and installs the swapped character's expressions for the triggers (the work that builds, allocates or waits).
//!
//! The performer's handle ([`PerformHandle`]) and the input ([`LiveInput`](super::input::LiveInput)) are the ops'
//! side; the frame thread never waits on them. Nothing in `frame` allocates in steady state beyond what the renderer
//! and the part runtimes cost (`tests/live.rs` counts).
//!
//! # One performer per member
//!
//! The subject's performer drives the stage. Every other performing cast member ([`add_member`](LiveStage::add_member))
//! has a performer of its own, stepped on the same clock in the same frame, with its own handle for the ops: its own
//! tracker or none, voice, triggers, live score and drive map (`docs/design/drivers.md`). Its pose and channels are
//! kept as the next frame's input for that member ([`member_input`](LiveStage::member_input)), which the stage draws
//! once it holds every member. The [`CastBook`] names them all, the subject first, for the ops ([`LiveCast`]).

use std::sync::Arc;

use omuzaki_budget::{Change, Scheduler};
use omuzaki_core::glam::Mat4;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{Quality, Renderer};

use omuzaki_stage_api::ids::MemberId;

use super::cast::{CastBook, LiveCast};
use super::frame::PerformFrame;
use super::host::{PerformControl, PerformError, PerformHandle, PerformHost, perform_channel};
use super::input::SourceSlot;
use super::Performer;
use crate::error::StageError;
use crate::frame::{FrameInput, FrameOutput, Stage};
use crate::scene::apply::{Mailbox, Owed};
use crate::scene::default::DEFAULT_MEMBER;
use crate::scene::resolve::ResolvedScene;
use crate::stages::FrameLoop;

/// What one live frame produced.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LiveFrame {
    /// The frame the stage rendered.
    pub output: FrameOutput,
    /// The change of quality the scheduler decided on this frame's report, for the app to show the person (§8). It
    /// applies from the next frame.
    pub change: Option<Change>,
}

/// A cast member performing beside the subject: its performer, the two ends of its channel, and the input of its next
/// frame.
struct LiveMember {
    id: MemberId,
    performer: Performer,
    control: PerformControl,
    handle: PerformHandle,
    frames: PerformFrame,
}

/// The live loop's state: the performer and its control, the stage, the input's source slot and the scheduler.
pub struct LiveStage<S: FrameLoop = Stage> {
    performer: Performer,
    control: PerformControl,
    handle: PerformHandle,
    stage: S,
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
    /// Set on resume: the next frame rebases the performer's clock on its `now`.
    resumed: bool,
    /// The stage's [`revision`](Stage::revision) the triggers' expressions were installed for.
    expressions_for: u64,
    /// The subject's member id.
    subject: MemberId,
    /// The members performing beside the subject.
    members: Vec<LiveMember>,
    /// Every performing member's host, the subject first, for the ops.
    cast: Arc<CastBook>,
}

impl<S: FrameLoop> LiveStage<S> {
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
        stage: S,
        sources: Arc<SourceSlot>,
        quality: Quality,
        budget: Seconds,
    ) -> Result<Self, PerformError> {
        let (handle, control) = perform_channel(&performer);
        let mut stage = stage;
        // The scheduler starts at `quality`, so the stage renders at it from the first frame.
        let scheduler = Scheduler::new(quality, budget);
        stage.set_quality(scheduler.quality());
        let live = Self {
            performer,
            control,
            handle,
            stage,
            frames: PerformFrame::new(FrameInput::default()),
            sources,
            scheduler,
            paused: false,
            scenes: Arc::new(Mailbox::new()),
            scene: ResolvedScene::new(),
            subject_root: Mat4::IDENTITY,
            subject_dirty: false,
            resumed: false,
            expressions_for: 0,
            subject: MemberId::new(DEFAULT_MEMBER).map_err(|_| PerformError::Unsupported)?,
            members: Vec::new(),
            cast: Arc::new(CastBook::new()),
        };
        live.cast.insert(live.subject.clone(), Arc::new(live.handle.clone()));
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
        let expressions = self.stage.trigger_expressions();
        self.handle.set_expressions(expressions, *self.performer.rest())
    }

    /// Runs one display frame at `now` (seconds on the loop's clock). Returns the frame, or `None` when no performer
    /// step was due and nothing was rendered.
    ///
    /// # Errors
    /// What [`Stage::service_variants`], [`Stage::tick`] or [`Stage::redraw`] reports. The performer has stepped; the next call carries on.
    pub fn frame(&mut self, renderer: &mut dyn Renderer, now: Seconds) -> Result<Option<LiveFrame>, StageError> {
        if std::mem::take(&mut self.resumed) {
            // The performer carries on from its last step: the paused time is not caught up.
            self.control.rebase(now);
            for member in &mut self.members {
                member.control.rebase(now);
            }
        }
        self.sources.swap_into(&mut self.performer);
        self.take_scene();
        let output = if self.paused {
            self.stage.redraw(renderer, self.frames.input())?
        } else {
            // The members step first, on the same clock, so a frame shows every member at the same step; a stage
            // that draws the cast takes each member's input for this frame.
            for member in &mut self.members {
                if member.frames.advance(&mut member.control, &mut member.performer, now) {
                    self.stage.set_member_input(&member.id, member.frames.input());
                }
            }
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

    /// The cold half of a display frame, which the loop runs once per iteration, after [`frame`](Self::frame) and
    /// outside its deadline (after presenting, or when idle): it moves a face variant switch along
    /// ([`Stage::service_variants`]: building, allocating and waiting happen here, never in `frame`), makes the ground disc
    /// a new scene owes ([`FrameLoop::pay_scene`]) and, when a swap
    /// has replaced the character, installs its expressions for the triggers ([`refresh_expressions`](Self::refresh_expressions)),
    /// so a reload that changes the face profile reaches the triggers with nobody repairing the table by hand.
    /// Returns whether the triggers' expressions were installed. A busy performer queue keeps the old expressions and
    /// the next call tries again.
    ///
    /// # Errors
    /// What [`Stage::service_variants`] or the ground disc reports. The performer carries on.
    pub fn service(&mut self, renderer: &mut dyn Renderer) -> Result<bool, StageError> {
        // Variant switches are staged outside the tick (`frame::face`), against the input the last frame showed.
        self.stage.service(renderer, self.frames.input())?;
        // A scene set since the last call may owe a ground disc; it is made here, where allocating is allowed.
        self.stage.pay_scene(renderer)?;
        Ok(self.refresh_expressions().unwrap_or(false))
    }

    /// Whether a variant swap replaced the character since the triggers' expressions were installed, so
    /// [`refresh_expressions`](Self::refresh_expressions) has something to do. Two integers compared: cheap anywhere.
    pub fn expressions_stale(&self) -> bool {
        self.stage.revision() != self.expressions_for
    }

    /// Installs the character's expressions again after a variant swap replaced it, so the triggers are baked against
    /// the face now showing (a swap whose face has the same expressions changes nothing visible). Returns whether it
    /// installed anything.
    ///
    /// This clones the expressions and takes the trigger table's lock, so it is not part of [`frame`](Self::frame):
    /// [`service`](Self::service) calls it where waiting and allocating are allowed, and `frame` itself allocates and
    /// locks nothing for it.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] when the performer has too many requests waiting, and
    /// [`PerformError::Disconnected`]; nothing changes then and the next call tries again.
    pub fn refresh_expressions(&mut self) -> Result<bool, PerformError> {
        let revision = self.stage.revision();
        if revision == self.expressions_for {
            return Ok(false);
        }
        self.install_expressions()?;
        self.expressions_for = revision;
        Ok(true)
    }

    /// Takes a posted scene, or a moved subject root, into the stage. Never waits: a poster holding the mailbox means
    /// the next frame tries again. Allocation-free.
    fn take_scene(&mut self) {
        if self.scenes.take_into(&mut self.scene) {
            self.subject_dirty = false;
            self.stage.set_scene(&self.scene, self.subject_root);
        } else if self.subject_dirty {
            // Only the subject moved: the scene stays as the stage has it, even before one was ever posted.
            self.subject_dirty = false;
            self.stage.set_subject_root(self.subject_root);
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
    /// for a value-only change, and again once the app has done the work and committed it with
    /// [`Stage::commit_scene`] (through [`stage_mut`](Self::stage_mut)); until then the same work stays owed.
    pub fn owed(&self) -> Owed {
        self.stage.owed()
    }

    /// Holds the performance (`paused`): every frame redraws the last one, with no step, so the output keeps
    /// rendering a still. Resuming carries on from the clock's `now`, skipping the paused time.
    pub fn set_paused(&mut self, paused: bool) {
        self.resumed |= self.paused && !paused;
        self.paused = paused;
    }

    /// Starts the performance over: the performer and its timeline go back to tick 0 (a recorded track that loops,
    /// a seek). The stage keeps running, so its frames keep their order.
    pub fn restart(&mut self) {
        self.control.reset(&mut self.performer);
        self.frames.restart();
        for member in &mut self.members {
            member.control.reset(&mut member.performer);
            member.frames.restart();
        }
    }

    /// Makes `member` the subject's id in the [cast](Self::cast) (the scene's subject member; `main` until set).
    pub fn set_subject_member(&mut self, member: MemberId) {
        if member == self.subject {
            return;
        }
        self.cast.remove(&self.subject);
        self.cast.insert(member.clone(), Arc::new(self.handle.clone()));
        self.cast.set_subject(&member);
        self.subject = member;
    }

    /// Adds `member` performing beside the subject, with `performer`, and returns its handle for the ops (also in the
    /// [cast](Self::cast)). A member of the same id is replaced; the subject's id is refused. When the stage draws the
    /// member ([`CastStage`](crate::cast::CastStage)), its frames reach it and its character's expressions go to its
    /// triggers; otherwise install them through the handle ([`PerformHost::set_expressions`]). Cold: allocates.
    ///
    /// # Errors
    /// [`PerformError::Unsupported`] for the subject's own id.
    pub fn add_member(&mut self, member: MemberId, performer: Performer) -> Result<PerformHandle, PerformError> {
        if member == self.subject {
            return Err(PerformError::Unsupported);
        }
        let (handle, control) = perform_channel(&performer);
        if let Some(expressions) = self.stage.trigger_expressions_of(&member) {
            handle.set_expressions(expressions, *performer.rest())?;
        }
        let input = FrameInput { pose: *performer.base(), ..FrameInput::default() };
        let entry = LiveMember { id: member.clone(), performer, control, handle: handle.clone(), frames: PerformFrame::new(input) };
        match self.members.iter_mut().find(|m| m.id == member) {
            Some(slot) => *slot = entry,
            None => self.members.push(entry),
        }
        self.cast.insert(member, Arc::new(handle.clone()));
        Ok(handle)
    }

    /// Stops `member` performing and gives its performer back. Cold.
    pub fn remove_member(&mut self, member: &MemberId) -> Option<Performer> {
        let at = self.members.iter().position(|m| &m.id == member)?;
        self.cast.remove(member);
        Some(self.members.remove(at).performer)
    }

    /// The input `member`'s next frame shows: its performer's pose and channels at the last step. The subject's is
    /// [`input`](Self::input).
    pub fn member_input(&self, member: &MemberId) -> Option<&FrameInput> {
        if member == &self.subject {
            return Some(self.frames.input());
        }
        self.members.iter().find(|m| &m.id == member).map(|m| m.frames.input())
    }

    /// The input of `member`'s next frame, to set its root, wind or goals.
    pub fn member_input_mut(&mut self, member: &MemberId) -> Option<&mut FrameInput> {
        if member == &self.subject {
            return Some(self.frames.input_mut());
        }
        self.members.iter_mut().find(|m| &m.id == member).map(|m| m.frames.input_mut())
    }

    /// The handle of `member`'s performer, the subject's included.
    pub fn member_handle(&self, member: &MemberId) -> Option<&PerformHandle> {
        if member == &self.subject {
            return Some(&self.handle);
        }
        self.members.iter().find(|m| &m.id == member).map(|m| &m.handle)
    }

    /// Every performing member's host, the subject first: inject it into the ops' services as `Arc<dyn LiveCast>`.
    pub fn cast(&self) -> Arc<dyn LiveCast> {
        Arc::clone(&self.cast) as Arc<dyn LiveCast>
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
    pub fn stage(&self) -> &S {
        &self.stage
    }

    /// The stage, to change its scene or face between frames.
    pub fn stage_mut(&mut self) -> &mut S {
        &mut self.stage
    }

    /// The frame-budget scheduler.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Takes the stage back, to release it from the renderer.
    pub fn into_stage(self) -> S {
        self.stage
    }
}

impl<S: FrameLoop> std::fmt::Debug for LiveStage<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveStage").field("paused", &self.paused).finish_non_exhaustive()
    }
}
