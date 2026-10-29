//! [`Performer`]: the fixed-step loop from a frame source to a humanoid pose.

use omuzaki_anim_api::{
    Animator, Bus, ChannelBuf, Cues, FrameSource, HumanoidPose, Layer, LayerStack, LifeLayer, PoseLayers, Reaches, Retarget,
    Tick, TrackFrame, TrackStatus,
};
use omuzaki_anim::grammar::GrammarStage;
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::drive::{self, DriveGate};
use super::score::{OverLayer, ScorePose};

/// The fixed step rate, steps per second.
pub const STEP_HZ: f32 = 60.0;
/// The most steps one [`Performer::advance`] runs. A longer stall skips ahead instead of catching up.
pub const MAX_STEPS_PER_ADVANCE: u32 = 4;
/// Seconds without a frame after which the source counts as lost.
pub const STALE_SOURCE_S: f64 = 0.25;

/// Smallest and largest `dt` the retarget is given (its contract).
const MIN_APPLY_DT: f32 = 1.0 / 120.0;
const MAX_APPLY_DT: f32 = 0.25;
/// Slack so a `now` that lands exactly on a step boundary is not lost to float rounding.
const BOUNDARY_EPS: f64 = 1e-9;

/// The stages a [`Performer`] runs, as trait objects.
pub struct PerformerParts {
    /// Where tracker frames come from.
    pub source: Box<dyn FrameSource>,
    /// Tracker frames to channels.
    pub retarget: Box<dyn Retarget>,
    /// Procedural life.
    pub life: Box<dyn LifeLayer>,
    /// Channels to a humanoid pose.
    pub animator: Box<dyn Animator>,
}

/// Whether a face is being tracked.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tracking {
    /// The source has not delivered a frame yet.
    NoFrames,
    /// The newest frame is fresh and sees a face.
    Tracking,
    /// The newest frame says the face is lost, or no frame has come for [`STALE_SOURCE_S`].
    Lost,
}

/// What one step produced, borrowed from the [`Performer`].
#[derive(Copy, Clone, Debug)]
pub struct PerformOutput<'a> {
    /// The step that ran.
    pub tick: Tick,
    /// The frame that arrived on this step; `None` on steps where the source had nothing new.
    pub frame: Option<&'a TrackFrame>,
    /// The final channels (sanitised) and who owns them: what the face painter reads.
    pub bus: &'a Bus,
    /// The animation pose: what the rig reads.
    pub pose: &'a HumanoidPose,
    /// Where the score's clips put the hands on the body: what the rig bends the arms to after the pose
    /// ([`FrameInput::reach`](crate::frame::FrameInput::reach)).
    pub reach: &'a Reaches,
    /// Whether the face is tracked.
    pub tracking: Tracking,
}

/// The perform loop. Hot path: [`Performer::advance`] and [`Performer::step`] allocate and lock nothing.
pub struct Performer {
    parts: PerformerParts,
    /// The anime grammar (spec FE-05), after the life layer with the same cues; `None` runs none.
    grammar: Option<GrammarStage>,
    stack: LayerStack,
    /// Which drivers move which channels (the member's drive map).
    drive: DriveGate,
    /// The live score's body clips, laid on the base pose before the animator.
    score_pose: ScorePose,
    /// The live score's acts made over tracking, laid on the bus after the life layer.
    score_over: OverLayer,
    bus: Bus,
    pose: HumanoidPose,
    base: HumanoidPose,
    cues: Cues,
    /// The `now` of the first `advance`; step `i` happens at pipeline time `i / STEP_HZ` after it.
    origin: Option<f64>,
    /// The next step `advance` will run.
    next_step: u64,
    skipped: u64,
    /// The newest frame, and the pipeline time it arrived at. Kept for stale detection and the lost frames' seq.
    last_frame: Option<TrackFrame>,
    /// Whether `last_frame` arrived on the step that just ran.
    arrived: bool,
    last_arrival: Seconds,
    /// Pipeline time of the last retarget call, for its `dt`.
    last_apply: Option<f64>,
    tracking: Tracking,
}

impl Performer {
    /// A performer showing `rest` (the resting expression) on `base` (the character's bind pose) until tracked.
    pub fn new(parts: PerformerParts, rest: ChannelBuf, base: HumanoidPose) -> Self {
        Self {
            parts,
            grammar: None,
            stack: LayerStack::new(rest),
            drive: DriveGate::OPEN,
            score_pose: ScorePose::default(),
            score_over: OverLayer::default(),
            bus: Bus::default(),
            pose: base,
            base,
            cues: Cues::default(),
            origin: None,
            next_step: 0,
            skipped: 0,
            last_frame: None,
            arrived: false,
            last_arrival: Seconds(0.0),
            last_apply: None,
            tracking: Tracking::NoFrames,
        }
    }

    /// The same performer with the anime grammar rules running after the life layer, with the life layer's cues: a
    /// smiling blink closes into `^`, a loud laugh squints, a pout puffs the cheeks (spec FE-05).
    #[must_use]
    pub fn with_grammar(mut self, grammar: GrammarStage) -> Self {
        self.grammar = Some(grammar);
        self
    }

    /// The grammar stage, to switch rules for another character ([`GrammarStage::set_config`]).
    pub fn grammar_mut(&mut self) -> Option<&mut GrammarStage> {
        self.grammar.as_mut()
    }

    /// Runs the steps due up to `now`; the first call sets the origin. At most [`MAX_STEPS_PER_ADVANCE`] per call,
    /// then it skips ahead (counted by [`Performer::skipped_steps`]). Returns the last step run, or `None` if none was
    /// due.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a non-negative step count far below 2^63
    pub fn advance(&mut self, now: Seconds) -> Option<PerformOutput<'_>> {
        let origin = *self.origin.get_or_insert(now.0);
        let elapsed = (now.0 - origin).max(0.0);
        // Steps with an index below `due` have come due.
        let due = ((elapsed * f64::from(STEP_HZ) + BOUNDARY_EPS).floor() as u64).saturating_add(1);
        let pending = due.saturating_sub(self.next_step);
        if pending == 0 {
            return None;
        }
        let run = pending.min(u64::from(MAX_STEPS_PER_ADVANCE));
        let last = self.next_step + run - 1;
        for index in self.next_step..last {
            self.run_step(Tick::at_rate(index, STEP_HZ));
        }
        self.run_step(Tick::at_rate(last, STEP_HZ));
        self.skipped += pending - run;
        self.next_step = due;
        Some(self.output(Tick::at_rate(last, STEP_HZ)))
    }

    /// Runs one step at `tick`, whatever the clock says (offline renders and tests drive this directly).
    pub fn step(&mut self, tick: Tick) -> PerformOutput<'_> {
        self.run_step(tick);
        self.output(tick)
    }

    /// Steps `advance` dropped because the loop fell behind by more than [`MAX_STEPS_PER_ADVANCE`].
    pub fn skipped_steps(&self) -> u64 {
        self.skipped
    }

    /// The trigger layer (hotkeys and timed expressions) of the stack.
    pub fn trigger_layer_mut(&mut self) -> &mut Layer {
        &mut self.stack.trigger
    }

    /// The score layer of the stack, the score's acts made over tracking and its body layers: what the live score
    /// writes before each step (acts from agents, scripts and the AI performer).
    pub fn score_mut(&mut self) -> (&mut Layer, &mut OverLayer, &mut ScorePose) {
        (&mut self.stack.score, &mut self.score_over, &mut self.score_pose)
    }

    /// Which drivers move which channels from the next step: the member's drive map (`perform::drive`).
    pub fn set_drive(&mut self, gate: DriveGate) {
        self.drive = gate;
    }

    /// The drive gate in use.
    pub fn drive(&self) -> &DriveGate {
        &self.drive
    }

    /// The audio (lip-sync) layer of the stack: what a voice writes before each step (`PerformHost::set_voice`).
    pub fn audio_layer_mut(&mut self) -> &mut Layer {
        &mut self.stack.audio
    }

    /// Sets the audio cues for the coming steps. `onset` is one-shot: it applies to the next step only.
    pub fn set_cues(&mut self, cues: Cues) {
        self.cues = cues;
    }

    /// The audio cues the coming step reads (`onset` stays set until a step has consumed it).
    pub fn cues(&self) -> &Cues {
        &self.cues
    }

    /// The retarget in use.
    pub fn retarget(&self) -> &dyn Retarget {
        &*self.parts.retarget
    }

    /// The retarget in use, for calibration and settings changes (control calls, between steps).
    pub fn retarget_mut(&mut self) -> &mut dyn Retarget {
        &mut *self.parts.retarget
    }

    /// Swaps the frame source and returns the old one, so a caller on the perform thread can hand it elsewhere to be
    /// dropped instead of freeing it here. The new source starts without frames, so the tracker layer is cleared until
    /// it speaks, and the retarget forgets the old source's held pose (its calibration and settings stay).
    pub fn set_source(&mut self, source: Box<dyn FrameSource>) -> Box<dyn FrameSource> {
        let old = std::mem::replace(&mut self.parts.source, source);
        self.parts.retarget.reset();
        self.stack.tracker.clear();
        self.last_frame = None;
        self.arrived = false;
        self.last_apply = None;
        self.tracking = Tracking::NoFrames;
        old
    }

    /// The resting expression the triggers blend onto.
    pub fn rest(&self) -> &ChannelBuf {
        &self.stack.rest
    }

    /// The base pose: the character's bind, which the animator layers onto.
    pub fn base(&self) -> &HumanoidPose {
        &self.base
    }

    /// Replaces the resting expression and the base pose.
    pub fn set_rest(&mut self, rest: ChannelBuf, base: HumanoidPose) {
        self.stack.rest = rest;
        self.base = base;
    }

    /// Back to the state after construction: the clock origin, the stages' state and the layers are forgotten. The
    /// source, the calibration and the settings stay.
    pub fn reset(&mut self) {
        self.parts.retarget.reset();
        self.parts.life.reset();
        if let Some(grammar) = self.grammar.as_mut() {
            grammar.reset();
        }
        self.parts.animator.reset();
        self.stack.tracker.clear();
        self.stack.audio.clear();
        self.stack.trigger.clear();
        self.stack.score.clear();
        self.score_over.clear();
        self.score_pose.clear();
        self.cues = Cues::default();
        self.origin = None;
        self.next_step = 0;
        self.skipped = 0;
        self.last_frame = None;
        self.arrived = false;
        self.last_apply = None;
        self.tracking = Tracking::NoFrames;
        self.pose = self.base;
    }

    fn output(&self, tick: Tick) -> PerformOutput<'_> {
        PerformOutput {
            tick,
            frame: if self.arrived { self.last_frame.as_ref() } else { None },
            bus: &self.bus,
            pose: &self.pose,
            reach: self.score_pose.reaches(),
            tracking: self.tracking,
        }
    }

    fn run_step(&mut self, tick: Tick) {
        self.track(tick);
        drive::resolve(&self.stack, &self.drive, &mut self.bus);
        self.parts.life.step(tick, &self.cues, &mut self.bus);
        // An act made over tracking wins on its channels over every driver and the life layer; the grammar still
        // reads the result, so its rules hold on a forced face too.
        self.score_over.apply(&mut self.bus.values);
        if let Some(grammar) = self.grammar.as_mut() {
            grammar.step(tick, &self.cues, &mut self.bus);
        }
        self.cues.onset = false;
        self.bus.values.sanitize();
        // The score's clips lie under the animator, so the tracked head and the breath still move a waving body.
        self.pose = self.base;
        PoseLayers(self.score_pose.layers()).resolve(&mut self.pose);
        self.parts.animator.animate(tick, &self.bus, &[], &mut self.pose);
    }

    /// Polls the source and feeds the retarget: the new frame, or a synthetic lost frame once the source has gone
    /// quiet, so the retarget's lost policy keeps running and hands the face to life.
    fn track(&mut self, tick: Tick) {
        self.arrived = false;
        if let Some(frame) = self.parts.source.poll(tick.time) {
            self.arrived = true;
            self.last_frame = Some(frame);
            self.last_arrival = tick.time;
            self.apply(tick, &frame);
        }
        let Some(newest) = self.last_frame else {
            self.tracking = Tracking::NoFrames;
            return;
        };
        let stale = tick.time.0 - self.last_arrival.0 > STALE_SOURCE_S;
        if stale {
            let lost = TrackFrame::lost(newest.seq, tick.time, newest.profile);
            self.apply(tick, &lost);
        }
        self.tracking = if stale || newest.status == TrackStatus::Lost { Tracking::Lost } else { Tracking::Tracking };
    }

    #[allow(clippy::cast_possible_truncation)] // a step gap of seconds fits f32
    fn apply(&mut self, tick: Tick, frame: &TrackFrame) {
        let since = self.last_apply.map_or(1.0 / STEP_HZ, |t| (tick.time.0 - t) as f32);
        self.last_apply = Some(tick.time.0);
        let dt = if since.is_finite() { since.clamp(MIN_APPLY_DT, MAX_APPLY_DT) } else { MIN_APPLY_DT };
        self.parts.retarget.apply(frame, dt, &mut self.stack.tracker);
    }
}

impl std::fmt::Debug for Performer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Performer")
            .field("next_step", &self.next_step)
            .field("skipped", &self.skipped)
            .field("tracking", &self.tracking)
            .finish_non_exhaustive()
    }
}
