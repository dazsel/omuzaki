//! [`PerformHost`]: what the ops (and the UI) can ask of the live performer, across the thread boundary.
//!
//! The [`Performer`] lives on the perform thread and ops run elsewhere, so they never touch it. [`perform_channel`]
//! makes the two ends: a [`PerformHandle`] (implements [`PerformHost`]; the App injects it as `Arc<dyn PerformHost>`)
//! and a [`PerformControl`] the perform thread calls instead of [`Performer::advance`].
//!
//! The perform thread never locks, allocates or frees in steady state:
//! - commands arrive through a bounded queue, at most [`MAX_COMMANDS_PER_ADVANCE`] per call, and carry their data
//!   ready-made (the handle builds a full [`RetargetConfig`] off-thread);
//! - the tracking state, capture progress and frame rate go back through one atomic word;
//! - the calibration in use goes back through a lock-free latest-value mailbox, and each applied settings request is
//!   acknowledged by its sequence number.
//!
//! The one exception is the retarget's own `set_config`, which its contract allows to allocate between frames.
//!
//! # The voice
//!
//! [`PerformHost::set_voice`] attaches a voice: the reading end of an analyser's mailbox (`omuzaki-track-voice`) and
//! the lip sync baked from the character's visemes on the handle's side. Before every step the control takes the
//! newest voice frame and writes the performer's audio layer and the life layer's cues (FE-08, PF-08), so any loop
//! built on the control (the stage's [`LiveStage`](super::live::LiveStage), the studio's perform thread) moves the
//! mouth with the voice. [`PerformHost::set_expressions`] re-bakes the visemes with the triggers. Attaching,
//! detaching and a new expression set are the only voice requests: they hand over, and drop, one boxed value each,
//! which is not steady state; the per-step work allocates nothing.
//!
//! # What is reported
//!
//! The handle keeps two views of the settings. **Requested** is the base of the next merge, so back-to-back changes
//! compose; it is also what [`PerformHost::set_retarget`] answers with, labelled as requested and numbered
//! ([`RetargetReceipt`]). **Applied** is what the performer has acknowledged, and it is the only view
//! [`PerformHost::status`] reports, with the number of the newest request it includes. The handle is made from the
//! performer ([`perform_channel`]), so both views start from the settings really in use.
//!
//! The calibration has the same two views: [`PerformHost::calibration`] is what the performer applied and
//! [`PerformHost::requested_calibration`] what has been asked for. Whatever saves state (`perform::settings`) saves
//! the requested view, because a request that is only queued is already a decision the person made; saving the applied
//! view would write the old value back over it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use omuzaki_anim::hands::HandRest;
use omuzaki_anim::poses::{ClipLibrary, PoseClip};
use omuzaki_anim::{
    MAX_NEUTRAL_FRAMES, MAX_TRIGGERS, MIN_NEUTRAL_FRAMES, NeutralCapture, NeutralError, TriggerFades, TriggerSlot,
};
use omuzaki_anim_api::{
    ARKIT_COUNT, BodyStyle, BoneMask, Calibration, Channel, ChannelBuf, ChannelMask, Cues, DeviceProfile, HeadPose,
    Impact, RetargetConfig, Tick, TrackStatus,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_stage_api::drive::DriveMap;
use omuzaki_track_api::pose_out::PoseTap;
use omuzaki_track_voice::{LipSync, LipSyncConfig, VoiceReader};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::triggers::{
    BindBatch, BindOutcome, Bound, MAX_KEY_CHARS, Rebaked, TriggerBinding, TriggerBook, TriggerInfo, TriggerLayer, check_key,
};
use super::drive::DriveGate;
use super::score::{Act, Envelope, Lane, LiveScore, MAX_ACTS, Motion};
use super::{MAX_STEPS_PER_ADVANCE, PerformOutput, Performer, STEP_HZ, Tracking};

/// Commands the queue holds before [`PerformError::QueueFull`].
pub const COMMAND_QUEUE_LEN: usize = 16;
/// Commands one [`PerformControl::advance`] takes from the queue. A producer can refill the queue as fast as it is
/// drained, so this bounds the work before the performer steps; the rest wait for the next call.
pub const MAX_COMMANDS_PER_ADVANCE: usize = 4;
/// Ticks (steps of the control's timeline, 1/[`STEP_HZ`] s each, skipped ones included) a neutral capture waits for its
/// next tracked frame before it gives up: 10 s.
pub const CAPTURE_STALL_STEPS: u64 = 600;
/// How often [`PerformHandle::await_retarget`] looks for the performer's acknowledgement.
#[cfg(not(target_arch = "wasm32"))]
const REPLY_POLL: Duration = Duration::from_millis(1);
/// Slack so a `now` that lands exactly on a step boundary is not lost to float rounding (as [`Performer::advance`]).
const BOUNDARY_EPS: f64 = 1e-9;
/// Weight of the newest arrival gap in the [`PerformStatus::source_fps`] average.
const FPS_SMOOTHING: f32 = 0.1;
/// One step, in seconds: the time the trigger fades advance by each step.
const STEP_SECS: f32 = 1.0 / STEP_HZ;
/// Radians the head turns, side to side, for a full look with the head following all the way (`head = 1`).
pub const LOOK_HEAD_YAW_RAD: f32 = 0.6;
/// Radians the head tips, up and down, for a full look with the head following all the way.
pub const LOOK_HEAD_PITCH_RAD: f32 = 0.35;
/// Acts [`PerformHost::acts`] remembers beyond the ones playing: the ones still queued.
const ACT_BOOK_LEN: usize = MAX_ACTS + COMMAND_QUEUE_LEN;

/// What the ops (and the UI) can ask of the live performer. Requests are queued for the perform thread. For
/// [`calibrate`](Self::calibrate) and [`set_calibration`](Self::set_calibration) a reply means "accepted", not
/// "applied"; [`status`](Self::status) shows what has happened since. That includes [`set_retarget`](Self::set_retarget):
/// nothing here waits, because the ops call it while the executor holds the document lock.
pub trait PerformHost: Send + Sync {
    /// Starts a neutral capture of `frames` tracked frames (clamped to the capture's own limits); the result applies
    /// itself when done.
    ///
    /// # Errors
    /// [`PerformError::CalibrationRunning`] while a capture is running, [`PerformError::QueueFull`] or
    /// [`PerformError::Disconnected`].
    fn calibrate(&self, frames: usize) -> Result<(), PerformError>;

    /// Queues a partial change and returns at once with a [`RetargetReceipt`]: the settings as requested, and the
    /// sequence number to look for. The change is merged into the settings requested so far, so back-to-back changes
    /// compose; a change made around the host, directly on the performer's retarget, is not seen. The performer
    /// applies it on a later step: [`status`](Self::status) reports the applied settings and
    /// [`retarget_seq`](PerformStatus::retarget_seq), which reaches the receipt's `seq` once it has.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`]; nothing is queued then.
    fn set_retarget(&self, change: RetargetChange) -> Result<RetargetReceipt, PerformError>;

    /// Replaces the calibration in use (a saved profile loading, say).
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`].
    fn set_calibration(&self, calibration: Calibration) -> Result<(), PerformError>;

    /// The calibration the performer is using (the last one it applied).
    fn calibration(&self) -> Calibration;

    /// The calibration once every [`set_calibration`](Self::set_calibration) made so far has run: what a caller that
    /// just set one (and may be about to save it) means, where [`calibration`](Self::calibration) shows only what the
    /// performer has applied. A capture that is still running is not a request: its result does not exist yet.
    fn requested_calibration(&self) -> Calibration {
        self.calibration()
    }

    /// The status the perform thread published last. Reports applied state only. Its parts are read one after the
    /// other, so they can describe slightly different instants (see [`PerformStatus`]).
    fn status(&self) -> PerformStatus;

    /// Replaces the expression library the triggers bake against, and the rest face they blend onto. Every bound key is
    /// re-baked; a key whose expression is no longer in the set stays bound and is listed as missing.
    ///
    /// A key held or toggled on keeps showing its expression across a reload that leaves the expression in the set,
    /// even with new values. A key whose expression goes missing stops showing it, and must be pressed again once the
    /// expression is back: presses of a missing key are not sent, so nothing says whether the key is still down.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`]; nothing changes then.
    fn set_expressions(&self, _set: ExpressionSet, _rest: ChannelBuf) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    /// Attaches a voice, replacing one attached before, or detaches it with `None`. From the next step the newest frame
    /// in `voice` (from a `VoiceListener` or any analyser) opens the mouth through the character's visemes (the
    /// expressions of the last [`set_expressions`](Self::set_expressions)) and gives the life layer its loudness and
    /// onsets; `config` sets how the mouth follows. Detaching withdraws the audio layer and quiets the cues.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`]; nothing changes then.
    fn set_voice(&self, _voice: Option<VoiceReader>, _config: LipSyncConfig) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    /// Binds `binding.key` to an expression, replacing a binding of the same key. A key that is not in the set is kept
    /// and listed as missing; the ops refuse such a key before they get here.
    ///
    /// # Errors
    /// [`PerformError::InvalidKey`] for a key that is not 1 to 32 characters, [`PerformError::TooManyTriggers`] when
    /// every slot holds another key, or [`PerformError::QueueFull`] or [`PerformError::Disconnected`].
    fn bind_trigger(&self, _binding: TriggerBinding) -> Result<BindOutcome, PerformError> {
        Err(PerformError::Unsupported)
    }

    /// Binds like [`bind_trigger`](Self::bind_trigger), but refuses an expression that is not in the set, checking and
    /// binding under one lock so a reload cannot remove it in between.
    ///
    /// # Errors
    /// [`PerformError::UnknownExpression`] for an expression the set does not have, and the errors of
    /// [`bind_trigger`](Self::bind_trigger).
    fn bind_trigger_checked(&self, _binding: TriggerBinding) -> Result<BindOutcome, PerformError> {
        Err(PerformError::Unsupported)
    }

    /// Binds every one of `bindings` as [`bind_trigger`](Self::bind_trigger) would, in order, in one request: they take
    /// one place in the performer's queue however many there are, so a full set of 16 can be restored before the
    /// performer has taken a step. Either all of them bind or none does.
    ///
    /// # Errors
    /// As [`bind_trigger`](Self::bind_trigger), for the first binding that fails.
    fn bind_triggers(&self, _bindings: Vec<TriggerBinding>) -> Result<Vec<BindOutcome>, PerformError> {
        Err(PerformError::Unsupported)
    }

    /// The retarget settings once every request made so far has run: what a caller that just changed them (and may be
    /// about to save them) means, where [`status`](Self::status) shows only what the performer has applied.
    fn requested_retarget(&self) -> RetargetConfig {
        self.status().retarget
    }

    /// Removes the binding of `key`. Returns whether there was one.
    ///
    /// # Errors
    /// [`PerformError::InvalidKey`], [`PerformError::QueueFull`] or [`PerformError::Disconnected`].
    fn unbind_trigger(&self, _key: &str) -> Result<bool, PerformError> {
        Err(PerformError::Unsupported)
    }

    /// Presses (`down`) or releases `key`. Returns whether the key shows something: `false` for a missing expression,
    /// where pressing does nothing.
    ///
    /// # Errors
    /// [`PerformError::UnknownKey`] when no trigger is bound to `key`, or [`PerformError::QueueFull`] or
    /// [`PerformError::Disconnected`].
    fn press(&self, _key: &str, _down: bool) -> Result<bool, PerformError> {
        Err(PerformError::Unsupported)
    }

    /// From the next step, publishes each step's pose and channels into `tap` (PF-04's VMC out), or stops with `None`.
    /// The caller keeps its own reference to the tap, so the perform thread never frees one.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`]; nothing changes then.
    fn set_pose_tap(&self, _tap: Option<Arc<dyn PoseTap>>) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    /// Every binding, with whether it is showing and whether it is missing.
    fn triggers(&self) -> Vec<TriggerInfo> {
        Vec::new()
    }

    /// Plays `act` on the score layer from the next step, coming and going as `envelope` says, and returns its id.
    /// It takes over from the earlier acts of its kind that move the same channels or bones (a second expression
    /// replaces the first). It is baked against the character's expressions and rest face as they are now.
    ///
    /// # Errors
    /// [`PerformError::UnknownExpression`] or [`PerformError::UnknownClip`] for a name the character or the clip
    /// library lacks, or [`PerformError::QueueFull`] or [`PerformError::Disconnected`]; nothing plays then.
    fn act(&self, _act: ActRequest, _envelope: Envelope) -> Result<u64, PerformError> {
        Err(PerformError::Unsupported)
    }

    /// Releases act `id`, or every act with `None`: each fades out over its own fade-out from where it is. An id that
    /// has ended already releases nothing.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`].
    fn release_act(&self, _id: Option<u64>) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    /// The acts playing or about to, oldest first.
    fn acts(&self) -> Vec<ActInfo> {
        Vec::new()
    }

    /// Which drivers move which channels from the next step (the member's drive map, `docs/design/drivers.md`).
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`]; nothing changes then.
    fn set_drive(&self, _drive: DriveMap) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    /// The drive map last set: every driver at full strength until one is.
    fn drive(&self) -> DriveMap {
        DriveMap::open()
    }

    /// How the body moves on its own from the next step (the living body's dials, `perform.set_body`).
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`]; nothing changes then.
    fn set_body(&self, _style: BodyStyle) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    /// The body style last set, or `None` before one is.
    fn body(&self) -> Option<BodyStyle> {
        None
    }

    /// Plays a physical reaction on the body from the next step (`act.react`): a blow or a fall, back on the
    /// performed pose after the impact's recovery time.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`]; nothing plays then.
    fn react(&self, _impact: Impact) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    /// The expression library the triggers bake against (empty until [`set_expressions`](Self::set_expressions)).
    fn expressions(&self) -> ExpressionSet {
        ExpressionSet::default()
    }
}

/// A partial change to the retarget's settings: only the fields that are `Some` change.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetargetChange {
    /// The smoothing dial, `0` raw to `1` very smooth.
    pub smoothing: Option<f32>,
    /// Exaggeration, `1` literal.
    pub exaggeration: Option<f32>,
    /// Multiplier on head angles.
    pub head_gain: Option<f32>,
    /// Swap left and right.
    pub mirror: Option<bool>,
}

impl RetargetChange {
    /// `config` with this change applied.
    fn apply_to(self, config: &mut RetargetConfig) {
        if let Some(v) = self.smoothing {
            config.smoothing = v;
        }
        if let Some(v) = self.exaggeration {
            config.exaggeration = v;
        }
        if let Some(v) = self.head_gain {
            config.head_gain = v;
        }
        if let Some(v) = self.mirror {
            config.mirror = v;
        }
    }
}

/// What an act shows, as the ops ask for it ([`PerformHost::act`]). The handle bakes it against the character's
/// expressions and rest face before it is queued, so the perform thread only blends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum ActRequest {
    /// One of the character's expressions.
    Expression {
        /// The expression.
        expression: ExpressionId,
        /// Strength, `0..=1`.
        intensity: f32,
        /// It wins over the tracker, lip sync and the triggers on its channels ([`Act::over_tracking`]).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        over_tracking: bool,
    },
    /// Where the eyes point, in look units (`x` toward the character's left, `y` up, `1` a full look), with the head
    /// following by `head` (`0` the eyes alone, `1` the head turns [`LOOK_HEAD_YAW_RAD`] for a full look).
    Look {
        /// Toward the character's left, `-1..=1`.
        x: f32,
        /// Up, `-1..=1`.
        y: f32,
        /// How much the head follows, `0..=1`.
        head: f32,
    },
    /// Channels to values (each held to its channel's range).
    Channels {
        /// The channels and their values.
        values: Vec<(Channel, f32)>,
        /// It wins over the tracker, lip sync and the triggers on its channels ([`Act::over_tracking`]).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        over_tracking: bool,
    },
    /// A body clip, from its start: the one given, or the clip library's of that id.
    Clip {
        /// The clip's id.
        clip: String,
        /// How strongly it moves the body, `0..=1`.
        weight: f32,
        /// The clip itself when the caller found it (a character's own clip, `clip.set`); `None` plays the library's
        /// clip of that id.
        #[serde(skip)]
        #[schemars(skip)]
        def: Option<Arc<PoseClip>>,
    },
}

impl ActRequest {
    /// The clip library's clip `id`, at `weight`.
    pub fn clip(id: impl Into<String>, weight: f32) -> Self {
        Self::Clip { clip: id.into(), weight, def: None }
    }

    /// `clip` itself (a character's own clip, or any other), at `weight`.
    pub fn clip_def(clip: Arc<PoseClip>, weight: f32) -> Self {
        Self::Clip { clip: clip.id.clone(), weight, def: Some(clip) }
    }
}

/// An act playing or about to ([`PerformHost::acts`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ActInfo {
    /// Its id, for [`PerformHost::release_act`].
    pub id: u64,
    /// What it shows.
    pub act: ActRequest,
    /// How it comes and goes, as it plays (a one-shot clip holds for its own length).
    pub envelope: Envelope,
}

/// Where the neutral capture stands.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CalibrationState {
    /// No capture has been asked for.
    Idle,
    /// Collecting tracked frames.
    Capturing {
        /// Tracked frames held so far.
        frames: usize,
        /// Tracked frames wanted.
        of: usize,
    },
    /// The last capture finished and its calibration is in use.
    Done {
        /// Tracked frames it was taken over.
        frames: usize,
    },
    /// The last capture gave up: the face was not seen for [`CAPTURE_STALL_STEPS`] ticks (10 s).
    Failed {
        /// Tracked frames held when it gave up.
        got: usize,
        /// Tracked frames wanted.
        need: usize,
    },
}

/// The answer to a queued settings change: what was asked for, not what the performer is using.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetargetReceipt {
    /// The request's number. [`PerformStatus::retarget_seq`] reaches it once the performer has applied the request.
    pub seq: u64,
    /// The full settings the request asks for: the change merged into the settings requested so far.
    pub requested: RetargetConfig,
}

/// What the performer last published, read by [`PerformHost::status`].
///
/// Each field is a consistent snapshot of its own: the tracking state, the capture state and the frame rate come from
/// one atomic word (they describe the same instant), and `retarget` is the settings the performer had acknowledged
/// when it was read. The two parts are read one after the other, so they can describe different instants: a status
/// read just as a settings change is applied can show the old tracking state with the new settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PerformStatus {
    /// Whether a face is tracked.
    pub tracking: Tracking,
    /// The neutral capture.
    pub calibration: CalibrationState,
    /// The settings the performer has applied.
    pub retarget: RetargetConfig,
    /// The newest [`RetargetReceipt::seq`] those settings include; `0` before any request. A request has been applied
    /// once this reaches its `seq`.
    pub retarget_seq: u64,
    /// Tracker frames per second as the performer sees them (a moving average; `0` before the first frame).
    pub source_fps: f32,
}

/// Why a request to the performer was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PerformError {
    /// The perform thread has stopped (its [`PerformControl`] is gone).
    #[error("the performer is not running")]
    Disconnected,
    /// Every trigger slot holds another key, so a new key cannot be bound: unbind one first.
    #[error("all {MAX_TRIGGERS} trigger slots are taken; unbind a key first")]
    TooManyTriggers,
    /// No trigger is bound to the key.
    #[error("no trigger is bound to that key")]
    UnknownKey,
    /// The key is not 1 to 32 printable characters.
    #[error("a trigger key is 1 to {MAX_KEY_CHARS} printable characters")]
    InvalidKey,
    /// The expression is not in the character's expression set.
    #[error("no such expression on this character")]
    UnknownExpression,
    /// The clip is not in the clip library.
    #[error("no such clip in the clip library")]
    UnknownClip,
    /// A neutral capture is already running.
    #[error("a calibration is already running")]
    CalibrationRunning,
    /// [`PerformHandle::await_retarget`] gave up: the performer did not apply the change in time (its loop is paused
    /// or very slow). The change stays queued and applies when the loop runs.
    #[error("the performer did not apply the change in time")]
    Stalled,
    /// Waiting on the performer is not available in the web build, which has no thread and no clock to wait with
    /// ([`PerformHandle::await_retarget`]). Only produced on `wasm32`.
    #[error("live performance isn't available in the web build")]
    Unsupported,
    /// The performer has not caught up with the requests already queued (the queue holds [`COMMAND_QUEUE_LEN`]).
    #[error("the performer has too many requests waiting; try again in a moment")]
    QueueFull,
}

/// A request on its way to the perform thread. Each carries everything the perform thread needs, already built, so it
/// allocates nothing to act on one.
// `Calibration` is large, but the queue's slots are allocated once with the channel: boxing it would allocate per
// request, and free on the perform thread.
#[allow(clippy::large_enum_variant)]
enum Command {
    Calibrate(usize),
    /// The full config the handle built, numbered so the performer can acknowledge it.
    Config {
        seq: u64,
        config: RetargetConfig,
    },
    Calibration(Calibration),
    /// One trigger slot, baked on the control side; `None` empties it.
    Trigger { index: usize, slot: Option<TriggerSlot>, generation: u64 },
    /// Every trigger slot whose values changed, from [`PerformHost::set_expressions`], and the voice's lip sync baked
    /// from the new set when a voice is attached.
    Triggers(Rebaked, Option<Box<LipSync>>),
    /// A voice to attach, or `None` to detach, from [`PerformHost::set_voice`].
    Voice(Option<Box<Voice>>),
    /// A batch of binds in one request, from [`PerformHost::bind_triggers`].
    BindBatch(BindBatch),
    /// A key press (`down`) or release on a trigger slot.
    Press { index: usize, down: bool },
    /// Where to publish each step's pose, or `None` to stop, from [`PerformHost::set_pose_tap`].
    PoseTap(Option<Arc<dyn PoseTap>>),
    /// An act for the live score, baked on the handle side, from [`PerformHost::act`].
    Act { id: u64, act: Act },
    /// Releases one act, or all with `None`, from [`PerformHost::release_act`].
    Release(Option<u64>),
    /// The drive gate, from [`PerformHost::set_drive`].
    Drive(DriveGate),
    /// The living body's dials, from [`PerformHost::set_body`].
    Body(BodyStyle),
    /// A physical reaction, from [`PerformHost::react`].
    React(Impact),
}

/// An attached voice, as the control holds it: the analyser's mailbox and the lip sync over the character's visemes.
struct Voice {
    reader: VoiceReader,
    lips: LipSync,
}

/// The handle's two views of the settings (see the module docs). Behind a lock only the handle side takes.
struct RetargetBook {
    /// The settings once every request made so far has run: the base of the next merge.
    requested: RetargetConfig,
    /// The settings the performer has acknowledged, and the request number they include.
    applied: RetargetConfig,
    applied_seq: u64,
    /// The last sequence number handed out.
    last_seq: u64,
    /// The calibration of the newest `set_calibration`, and how many were queued: while that is more than the
    /// performer has applied ([`Shared::calibrations_applied`]), this is the calibration once the queue has run.
    requested_calibration: Option<Calibration>,
    calibrations_queued: u64,
    /// Requests the performer has not acknowledged yet, with the config each asked for. At most the queue's length.
    in_flight: VecDeque<(u64, RetargetConfig)>,
}

impl RetargetBook {
    /// Moves every acknowledged request from `in_flight` into `applied`.
    fn settle(&mut self, acknowledged: u64) {
        while self.in_flight.front().is_some_and(|(seq, _)| *seq <= acknowledged) {
            if let Some((seq, config)) = self.in_flight.pop_front() {
                (self.applied, self.applied_seq) = (config, seq);
            }
        }
    }
}

/// The record both ends share.
struct Shared {
    /// Tracking, capture state and frame rate in one word (see [`Live`]), so a read is always consistent.
    live: AtomicU64,
    /// Set by the handle when it queues a capture, cleared by the control when the capture ends or the control drops.
    capturing: AtomicBool,
    /// Cleared when the control drops: nothing more will be applied.
    running: AtomicBool,
    /// The newest settings request the performer has applied.
    acknowledged: AtomicU64,
    /// The calibration the performer is using.
    calibration: CalibrationMailbox,
    /// How many `set_calibration` requests the performer has applied.
    calibrations_applied: AtomicU64,
    /// For each slot, the generation of the binding whose expression is shown or wanted, or 0
    /// (`TriggerLayer::active_generations`), published by the control each step. A binding is active when its own
    /// generation is the one published.
    active_triggers: [AtomicU64; MAX_TRIGGERS],
    /// Handle side only; the control never touches it.
    book: Mutex<RetargetBook>,
    /// The trigger table. Handle side only, like `book`: the control receives baked slots.
    triggers: Mutex<TriggerBook>,
    /// The attached voice's settings, or `None` with no voice. Handle side only; taken after `triggers`.
    voice: Mutex<Option<LipSyncConfig>>,
    /// The ids of the acts playing, oldest first, `0` past the last: published by the control each step.
    active_acts: [AtomicU64; MAX_ACTS],
    /// The newest act id the control has taken from the queue.
    acts_taken: AtomicU64,
    /// The last act id handed out.
    last_act: AtomicU64,
    /// The acts asked for that may still play, oldest first. Handle side only; taken after `triggers`.
    act_book: Mutex<VecDeque<ActInfo>>,
    /// The drive map last set. Handle side only.
    drive: Mutex<DriveMap>,
    /// The body style last set. Handle side only.
    body: Mutex<Option<BodyStyle>>,
    /// The clips of the acts handed to the control, kept until the control has dropped its own reference, so the
    /// perform thread never frees a clip when an act ends. Handle side only; pruned on each act.
    clips: Mutex<Vec<Arc<PoseClip>>>,
}

/// Locks `m`; a panic elsewhere cannot leave these plain records half-written, so a poisoned lock is still good.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One calibration, readable from any thread while the perform thread overwrites it: a latest-value mailbox. The
/// writer never waits and never fails (a write replaces the value), and a reader retries until it saw no write in
/// progress, so it can neither tear nor see a stale newest value. It is a seqlock over atomic words, which needs no
/// `unsafe`.
struct CalibrationMailbox {
    /// Odd while a write is in progress, and increases with each write.
    seq: AtomicU64,
    words: [AtomicU32; CALIBRATION_WORDS],
}

/// A calibration as words: the profile, a range (two) for each `ARKit` channel, the head rest angles, whether it has a
/// position, and the position.
const CALIBRATION_WORDS: usize = 1 + 2 * ARKIT_COUNT + 3 + 1 + 3;

impl CalibrationMailbox {
    fn new(calibration: &Calibration) -> Self {
        let mailbox = Self { seq: AtomicU64::new(0), words: std::array::from_fn(|_| AtomicU32::new(0)) };
        mailbox.write(calibration);
        mailbox
    }

    /// Replaces the value. Only the perform thread (or whoever holds the control) calls this, so writes never overlap.
    fn write(&self, calibration: &Calibration) {
        let words = Self::encode(calibration);
        let before = self.seq.load(Ordering::Relaxed);
        self.seq.store(before.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (slot, word) in self.words.iter().zip(words) {
            slot.store(word, Ordering::Relaxed);
        }
        self.seq.store(before.wrapping_add(2), Ordering::Release);
    }

    fn read(&self) -> Calibration {
        loop {
            let first = self.seq.load(Ordering::Acquire);
            if first.is_multiple_of(2) {
                let mut words = [0_u32; CALIBRATION_WORDS];
                for (word, slot) in words.iter_mut().zip(&self.words) {
                    *word = slot.load(Ordering::Relaxed);
                }
                fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == first {
                    return Self::decode(&words);
                }
            }
            std::thread::yield_now();
        }
    }

    fn encode(calibration: &Calibration) -> [u32; CALIBRATION_WORDS] {
        let mut words = [0_u32; CALIBRATION_WORDS];
        let mut next = words.iter_mut();
        let mut put = |value: u32| {
            if let Some(word) = next.next() {
                *word = value;
            }
        };
        put(u32::from(calibration.profile == DeviceProfile::Webcam));
        for info in Channel::arkit() {
            let range = calibration.range(info.channel);
            put(range.lo.to_bits());
            put(range.hi.to_bits());
        }
        let head = calibration.head_rest;
        put(head.yaw.to_bits());
        put(head.pitch.to_bits());
        put(head.roll.to_bits());
        let position = head.position.unwrap_or(Vec3::ZERO);
        put(u32::from(head.position.is_some()));
        put(position.x.to_bits());
        put(position.y.to_bits());
        put(position.z.to_bits());
        words
    }

    fn decode(words: &[u32; CALIBRATION_WORDS]) -> Calibration {
        let mut next = words.iter().copied();
        let mut take = || next.next().unwrap_or(0);
        let profile = if take() == 0 { DeviceProfile::Arkit } else { DeviceProfile::Webcam };
        let mut calibration = Calibration::identity(profile);
        for info in Channel::arkit() {
            let (lo, hi) = (f32::from_bits(take()), f32::from_bits(take()));
            calibration.set_range(info.channel, omuzaki_anim_api::ChannelRange { lo, hi });
        }
        let (yaw, pitch, roll) = (f32::from_bits(take()), f32::from_bits(take()), f32::from_bits(take()));
        let has_position = take() != 0;
        let position = Vec3::new(f32::from_bits(take()), f32::from_bits(take()), f32::from_bits(take()));
        calibration.head_rest = HeadPose { yaw, pitch, roll, position: has_position.then_some(position) };
        calibration
    }
}

/// The part of the status that changes every step, packed into a `u64`.
#[derive(Copy, Clone)]
struct Live {
    tracking: Tracking,
    calibration: CalibrationState,
    fps: f32,
}

// Layout: tracking 2 bits, calibration tag 2 bits, two 10-bit counts (a capture holds at most 600 frames), then the
// frame rate's bits in the upper half.
const TRACKING_SHIFT: u32 = 0;
const TAG_SHIFT: u32 = 2;
const A_SHIFT: u32 = 4;
const B_SHIFT: u32 = 14;
const FPS_SHIFT: u32 = 32;
const COUNT_MASK: u64 = (1 << 10) - 1;

impl Live {
    const START: Live = Live { tracking: Tracking::NoFrames, calibration: CalibrationState::Idle, fps: 0.0 };

    fn pack(self) -> u64 {
        let tracking = match self.tracking {
            Tracking::NoFrames => 0,
            Tracking::Tracking => 1,
            Tracking::Lost => 2,
        };
        let count = |n: usize| (n as u64).min(COUNT_MASK);
        let (tag, a, b) = match self.calibration {
            CalibrationState::Idle => (0, 0, 0),
            CalibrationState::Capturing { frames, of } => (1, count(frames), count(of)),
            CalibrationState::Done { frames } => (2, count(frames), 0),
            CalibrationState::Failed { got, need } => (3, count(got), count(need)),
        };
        tracking << TRACKING_SHIFT
            | tag << TAG_SHIFT
            | a << A_SHIFT
            | b << B_SHIFT
            | u64::from(self.fps.to_bits()) << FPS_SHIFT
    }

    #[allow(clippy::cast_possible_truncation)] // the fields are masked to 10 bits and the upper half is a u32
    fn unpack(word: u64) -> Self {
        let tracking = match (word >> TRACKING_SHIFT) & 0b11 {
            0 => Tracking::NoFrames,
            1 => Tracking::Tracking,
            _ => Tracking::Lost,
        };
        let a = ((word >> A_SHIFT) & COUNT_MASK) as usize;
        let b = ((word >> B_SHIFT) & COUNT_MASK) as usize;
        let calibration = match (word >> TAG_SHIFT) & 0b11 {
            0 => CalibrationState::Idle,
            1 => CalibrationState::Capturing { frames: a, of: b },
            2 => CalibrationState::Done { frames: a },
            _ => CalibrationState::Failed { got: a, need: b },
        };
        Self { tracking, calibration, fps: f32::from_bits((word >> FPS_SHIFT) as u32) }
    }
}

/// The ops' side. Cheap to clone; every clone talks to the same performer.
#[derive(Clone)]
pub struct PerformHandle {
    commands: SyncSender<Command>,
    shared: Arc<Shared>,
}

/// Frees the capture slot, and marks the control gone, when dropped. A field of [`PerformControl`] declared after its
/// receiver, so by the time it runs the queue is already disconnected: a `calibrate` that reserved the slot meanwhile
/// finds its send refused and releases the slot itself.
struct SlotGuard(Arc<Shared>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
        self.0.capturing.store(false, Ordering::Release);
    }
}

/// The two ends of the performer's control channel: hand the [`PerformHandle`] to the ops and keep the
/// [`PerformControl`] on the perform thread.
///
/// The handle is made from the `performer`, so it starts from the settings and calibration really in use: there is no
/// moment when it can answer from defaults. Make the channel once, before the loop starts; it takes the performer's
/// settings by clone, which is setup work.
pub fn perform_channel(performer: &Performer) -> (PerformHandle, PerformControl) {
    let (commands_tx, commands_rx) = sync_channel(COMMAND_QUEUE_LEN);
    let config = performer.retarget().config().clone();
    let shared = Arc::new(Shared {
        live: AtomicU64::new(Live::START.pack()),
        capturing: AtomicBool::new(false),
        running: AtomicBool::new(true),
        acknowledged: AtomicU64::new(0),
        calibration: CalibrationMailbox::new(performer.retarget().calibration()),
        calibrations_applied: AtomicU64::new(0),
        active_triggers: std::array::from_fn(|_| AtomicU64::new(0)),
        book: Mutex::new(RetargetBook {
            requested: config.clone(),
            applied: config,
            applied_seq: 0,
            last_seq: 0,
            requested_calibration: None,
            calibrations_queued: 0,
            in_flight: VecDeque::with_capacity(COMMAND_QUEUE_LEN),
        }),
        triggers: Mutex::new(TriggerBook::new()),
        voice: Mutex::new(None),
        active_acts: std::array::from_fn(|_| AtomicU64::new(0)),
        acts_taken: AtomicU64::new(0),
        last_act: AtomicU64::new(0),
        act_book: Mutex::new(VecDeque::with_capacity(ACT_BOOK_LEN + 1)),
        clips: Mutex::new(Vec::new()),
        drive: Mutex::new(DriveMap::open()),
        body: Mutex::new(None),
    });
    let control = PerformControl {
        commands: commands_rx,
        _slot: SlotGuard(Arc::clone(&shared)),
        shared: Arc::clone(&shared),
        // The capture's buffers are sized for the longest capture, once; a shorter one just stops pushing earlier.
        capture: NeutralCapture::new(MAX_NEUTRAL_FRAMES),
        running: None,
        origin: None,
        next_step: 0,
        skipped: 0,
        live: Live::START,
        fps: FpsMeter::default(),
        triggers: TriggerLayer::new(TriggerFades::default()),
        voice: None,
        head_rest: HeadRest::Waiting,
        pose_tap: None,
        score: Box::new(LiveScore::new()),
    };
    (PerformHandle { commands: commands_tx, shared }, control)
}

impl PerformHandle {
    fn send(&self, command: Command) -> Result<(), PerformError> {
        self.commands.try_send(command).map_err(|e| match e {
            TrySendError::Full(_) => PerformError::QueueFull,
            TrySendError::Disconnected(_) => PerformError::Disconnected,
        })
    }

    /// The book with the performer's acknowledgements taken in.
    fn book(&self) -> MutexGuard<'_, RetargetBook> {
        let mut book = lock(&self.shared.book);
        book.settle(self.shared.acknowledged.load(Ordering::Acquire));
        book
    }

    /// Waits up to `timeout` for the performer to apply request number `seq` (a [`RetargetReceipt::seq`]), and returns
    /// the settings it is then using. This blocks: call it from your own thread, never from an op (which runs under the
    /// document lock) and never from the perform thread. The ops read [`PerformHost::status`] instead.
    ///
    /// # Errors
    /// [`PerformError::Stalled`] after `timeout`, or [`PerformError::Disconnected`] if the control is gone before it
    /// applied the request. In the web build ([`PerformError::Unsupported`]) it always refuses.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn await_retarget(&self, seq: u64, timeout: Duration) -> Result<RetargetConfig, PerformError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.shared.acknowledged.load(Ordering::Acquire) >= seq {
                return Ok(self.book().applied.clone());
            }
            if !self.shared.running.load(Ordering::Acquire) {
                return Err(PerformError::Disconnected);
            }
            if Instant::now() >= deadline {
                return Err(PerformError::Stalled);
            }
            std::thread::sleep(REPLY_POLL);
        }
    }

    /// The web build has no thread to wait on and no clock to time a wait with (`Instant` and `thread::sleep` panic
    /// on `wasm32-unknown-unknown`), so waiting is refused, not attempted.
    ///
    /// # Errors
    /// Always [`PerformError::Unsupported`].
    #[cfg(target_arch = "wasm32")]
    pub fn await_retarget(&self, _seq: u64, _timeout: Duration) -> Result<RetargetConfig, PerformError> {
        Err(PerformError::Unsupported)
    }
}

impl PerformHandle {
    /// Binds under the book lock; with `checked`, an expression the set lacks is refused.
    fn bind_locked(&self, binding: TriggerBinding, checked: bool) -> Result<BindOutcome, PerformError> {
        let mut book = lock(&self.shared.triggers);
        if checked && !book.has_expression(&binding.expression) {
            return Err(PerformError::UnknownExpression);
        }
        let (slot, replaced) = book.place(&binding.key)?;
        let (shown, missing) = book.bake(&binding);
        let generation = book.next_generation();
        self.send(Command::Trigger { index: slot, slot: shown, generation })?;
        book.put(slot, Some(Bound { binding, missing, slot: shown, generation }));
        Ok(BindOutcome { slot, replaced })
    }
}

impl PerformHost for PerformHandle {
    fn calibrate(&self, frames: usize) -> Result<(), PerformError> {
        if self.shared.capturing.swap(true, Ordering::AcqRel) {
            return Err(PerformError::CalibrationRunning);
        }
        let sent = self.send(Command::Calibrate(frames));
        if sent.is_err() {
            self.shared.capturing.store(false, Ordering::Release);
        }
        sent
    }

    fn set_retarget(&self, change: RetargetChange) -> Result<RetargetReceipt, PerformError> {
        // Held across the send, so changes reach the performer in the order they are merged here.
        let mut book = self.book();
        let mut requested = book.requested.clone();
        change.apply_to(&mut requested);
        let seq = book.last_seq + 1;
        self.send(Command::Config { seq, config: requested.clone() })?;
        book.last_seq = seq;
        book.requested.clone_from(&requested);
        book.in_flight.push_back((seq, requested.clone()));
        Ok(RetargetReceipt { seq, requested })
    }

    fn set_calibration(&self, calibration: Calibration) -> Result<(), PerformError> {
        // Held across the send, like `set_retarget`, so the count and the last value agree with the queue's order.
        let mut book = self.book();
        self.send(Command::Calibration(calibration))?;
        book.requested_calibration = Some(calibration);
        book.calibrations_queued += 1;
        Ok(())
    }

    fn requested_calibration(&self) -> Calibration {
        let book = self.book();
        // Applied is read first: if it is behind, the queue still holds a request and the last one wins.
        let applied = self.shared.calibrations_applied.load(Ordering::Acquire);
        match book.requested_calibration {
            Some(requested) if book.calibrations_queued > applied => requested,
            _ => self.shared.calibration.read(),
        }
    }

    fn calibration(&self) -> Calibration {
        self.shared.calibration.read()
    }

    fn status(&self) -> PerformStatus {
        let live = Live::unpack(self.shared.live.load(Ordering::Acquire));
        let (retarget, retarget_seq) = {
            let book = self.book();
            (book.applied.clone(), book.applied_seq)
        };
        PerformStatus {
            tracking: live.tracking,
            calibration: live.calibration,
            retarget,
            retarget_seq,
            source_fps: live.fps,
        }
    }

    fn set_expressions(&self, expressions: ExpressionSet, rest: ChannelBuf) -> Result<(), PerformError> {
        let mut book = lock(&self.shared.triggers);
        let (next, changes) = book.with_expressions(expressions, &rest);
        let lips = lock(&self.shared.voice).map(|config| Box::new(LipSync::new(next.expression_set(), config)));
        self.send(Command::Triggers(changes, lips))?;
        *book = next;
        Ok(())
    }

    fn set_voice(&self, voice: Option<VoiceReader>, config: LipSyncConfig) -> Result<(), PerformError> {
        let book = lock(&self.shared.triggers);
        let mut attached = lock(&self.shared.voice);
        let voice = voice.map(|reader| Box::new(Voice { reader, lips: LipSync::new(book.expression_set(), config) }));
        let on = voice.is_some();
        self.send(Command::Voice(voice))?;
        *attached = on.then_some(config);
        Ok(())
    }

    fn bind_trigger(&self, binding: TriggerBinding) -> Result<BindOutcome, PerformError> {
        check_key(&binding.key)?;
        self.bind_locked(binding, false)
    }

    fn bind_trigger_checked(&self, binding: TriggerBinding) -> Result<BindOutcome, PerformError> {
        check_key(&binding.key)?;
        self.bind_locked(binding, true)
    }
    fn bind_triggers(&self, bindings: Vec<TriggerBinding>) -> Result<Vec<BindOutcome>, PerformError> {
        let mut book = lock(&self.shared.triggers);
        // Worked out on a copy, so a refusal part way changes nothing.
        let mut next = book.clone();
        let mut batch = BindBatch::new();
        let mut outcomes = Vec::with_capacity(bindings.len());
        for binding in bindings {
            check_key(&binding.key)?;
            let (slot, replaced) = next.place(&binding.key)?;
            let (shown, missing) = next.bake(&binding);
            let generation = next.next_generation();
            batch.set(slot, shown.as_ref(), generation);
            next.put(slot, Some(Bound { binding, missing, slot: shown, generation }));
            outcomes.push(BindOutcome { slot, replaced });
        }
        self.send(Command::BindBatch(batch))?;
        *book = next;
        Ok(outcomes)
    }

    fn requested_retarget(&self) -> RetargetConfig {
        self.book().requested.clone()
    }

    fn unbind_trigger(&self, key: &str) -> Result<bool, PerformError> {
        check_key(key)?;
        let mut book = lock(&self.shared.triggers);
        let Some((slot, _)) = book.find(key) else {
            return Ok(false);
        };
        self.send(Command::Trigger { index: slot, slot: None, generation: 0 })?;
        book.put(slot, None);
        Ok(true)
    }

    fn set_pose_tap(&self, tap: Option<Arc<dyn PoseTap>>) -> Result<(), PerformError> {
        self.send(Command::PoseTap(tap))
    }

    fn press(&self, key: &str, down: bool) -> Result<bool, PerformError> {
        let book = lock(&self.shared.triggers);
        let (slot, bound) = book.find(key).ok_or(PerformError::UnknownKey)?;
        if bound.missing {
            return Ok(false);
        }
        self.send(Command::Press { index: slot, down })?;
        Ok(true)
    }

    fn triggers(&self) -> Vec<TriggerInfo> {
        let book = lock(&self.shared.triggers);
        book.bound()
            .map(|(slot, bound)| TriggerInfo {
                binding: bound.binding.clone(),
                active: self.shared.active_triggers.get(slot).is_some_and(|published| {
                    published.load(Ordering::Acquire) == bound.generation
                }),
                missing: bound.missing,
            })
            .collect()
    }

    fn expressions(&self) -> ExpressionSet {
        lock(&self.shared.triggers).expressions()
    }

    fn act(&self, request: ActRequest, envelope: Envelope) -> Result<u64, PerformError> {
        let act = {
            let book = lock(&self.shared.triggers);
            bake_act(&request, envelope, book.expression_set(), book.rest())?
        };
        let mut acts = lock(&self.shared.act_book);
        if let Motion::Clip { clip, .. } = &act.motion {
            let mut clips = lock(&self.shared.clips);
            // A clip only this list still holds has left the score: dropping it here frees it off the perform thread.
            clips.retain(|kept| Arc::strong_count(kept) > 1);
            clips.push(Arc::clone(clip));
        }
        // Held across the send, so ids reach the performer in the order they are handed out.
        let id = self.shared.last_act.load(Ordering::Acquire) + 1;
        let envelope = act.envelope;
        self.send(Command::Act { id, act })?;
        self.shared.last_act.store(id, Ordering::Release);
        let playing = self.playing_acts();
        let taken = self.shared.acts_taken.load(Ordering::Acquire);
        acts.retain(|info| info.id > taken || playing.contains(&info.id));
        if acts.len() >= ACT_BOOK_LEN {
            acts.pop_front();
        }
        acts.push_back(ActInfo { id, act: request, envelope });
        Ok(id)
    }

    fn release_act(&self, id: Option<u64>) -> Result<(), PerformError> {
        self.send(Command::Release(id))
    }

    fn acts(&self) -> Vec<ActInfo> {
        let acts = lock(&self.shared.act_book);
        let playing = self.playing_acts();
        let taken = self.shared.acts_taken.load(Ordering::Acquire);
        acts.iter().filter(|info| info.id > taken || playing.contains(&info.id)).cloned().collect()
    }

    fn set_drive(&self, drive: DriveMap) -> Result<(), PerformError> {
        let mut held = lock(&self.shared.drive);
        self.send(Command::Drive(DriveGate::of(&drive)))?;
        *held = drive;
        Ok(())
    }

    fn drive(&self) -> DriveMap {
        lock(&self.shared.drive).clone()
    }

    fn set_body(&self, style: BodyStyle) -> Result<(), PerformError> {
        let mut held = lock(&self.shared.body);
        self.send(Command::Body(style))?;
        *held = Some(style);
        Ok(())
    }

    fn body(&self) -> Option<BodyStyle> {
        *lock(&self.shared.body)
    }

    fn react(&self, impact: Impact) -> Result<(), PerformError> {
        self.send(Command::React(impact))
    }
}

impl PerformHandle {
    /// The ids of the acts the performer published as playing.
    fn playing_acts(&self) -> [u64; MAX_ACTS] {
        std::array::from_fn(|i| self.shared.active_acts[i].load(Ordering::Acquire))
    }
}

/// `request` as the live score plays it, over `rest` and the character's `expressions`.
fn bake_act(
    request: &ActRequest,
    envelope: Envelope,
    expressions: &ExpressionSet,
    rest: &ChannelBuf,
) -> Result<Act, PerformError> {
    let envelope = envelope.clamped();
    let channels = |target: ChannelBuf, mask: ChannelMask| Motion::Channels { target, mask };
    let over_tracking = matches!(
        request,
        ActRequest::Expression { over_tracking: true, .. } | ActRequest::Channels { over_tracking: true, .. }
    );
    let (motion, lane, envelope) = match request {
        ActRequest::Expression { expression, intensity, .. } => {
            let intensity = unit(*intensity);
            let target = expressions
                .blend(rest, &[(expression, intensity)])
                .map_err(|_| PerformError::UnknownExpression)?;
            let mask = expressions.0.get(expression).map_or(ChannelMask::EMPTY, |e| e.deltas.0.keys().copied().collect());
            (channels(target, mask), Lane::Face, envelope)
        }
        ActRequest::Look { x, y, head } => {
            let (x, y, head) = (signed(*x), signed(*y), unit(*head));
            let mut target = *rest;
            target.set_gaze(x, y);
            let mut mask = ChannelMask::GAZE;
            if head > 0.0 {
                // Positive yaw turns toward the character's left, as positive x looks; positive pitch tips the face
                // down, so looking up tips it the other way.
                target[Channel::HEAD_YAW] = x * head * LOOK_HEAD_YAW_RAD;
                target[Channel::HEAD_PITCH] = -y * head * LOOK_HEAD_PITCH_RAD;
                mask.insert(Channel::HEAD_YAW);
                mask.insert(Channel::HEAD_PITCH);
            }
            (channels(target, mask), Lane::Look, envelope)
        }
        ActRequest::Channels { values, .. } => {
            let mut target = *rest;
            let mut mask = ChannelMask::EMPTY;
            for (channel, value) in values {
                let (lo, hi) = channel.range();
                target[*channel] = if value.is_finite() { value.clamp(lo, hi) } else { rest[*channel] };
                mask.insert(*channel);
            }
            (channels(target, mask), Lane::Channels, envelope)
        }
        ActRequest::Clip { clip, weight, def } => {
            let clip = if let Some(def) = def {
                Arc::clone(def)
            } else {
                let library = ClipLibrary::builtin().map_err(|_| PerformError::UnknownClip)?;
                Arc::new(library.get(clip).ok_or(PerformError::UnknownClip)?.clone())
            };
            // A clip that plays once ends with itself, unless the caller holds it for longer or less.
            let envelope = match envelope.hold {
                None if !clip.looped() => Envelope {
                    hold: Some((clip.length() - envelope.fade_in - envelope.fade_out).max(0.0)),
                    ..envelope
                },
                _ => envelope,
            };
            let motion =
                Motion::Clip { clip, hands: HandRest::nominal(), bones: BoneMask::ALL, weight: unit(*weight) };
            (motion, Lane::Body, envelope)
        }
    };
    Ok(Act { motion, envelope, lane, over_tracking })
}

/// `v` held to `0..=1`; not a number is 0.
fn unit(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// `v` held to `-1..=1`; not a number is 0.
fn signed(v: f32) -> f32 {
    if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 }
}

/// A capture in progress.
struct Running {
    /// Tracked frames wanted.
    frames: usize,
    /// Tracked frames held at the last step that made progress.
    held: usize,
    /// The control-timeline tick (see [`PerformControl`]) at which the capture last made progress, or started: `None`
    /// until the first [`PerformControl::advance`] that has due steps takes it, because a request that arrives after a
    /// pause must start at the current tick, not at the one before the catch-up.
    progress_tick: Option<u64>,
    /// Whether the frames are all in and only the performer's calibration is left to set.
    full: bool,
}

/// Tracker frames per second, as a moving average of the gaps between arrivals.
#[derive(Default)]
struct FpsMeter {
    last_arrival: Option<f64>,
    fps: f32,
}

impl FpsMeter {
    #[allow(clippy::cast_possible_truncation)] // a gap of seconds fits f32
    fn arrived(&mut self, at: f64) {
        if let Some(gap) = self.last_arrival.map(|last| at - last).filter(|gap| *gap > 0.0) {
            let instant = (1.0 / gap) as f32;
            self.fps = if self.fps == 0.0 { instant } else { self.fps + FPS_SMOOTHING * (instant - self.fps) };
        }
        self.last_arrival = Some(at);
    }
}

/// The perform thread's side: call [`advance`](Self::advance) where the loop would call [`Performer::advance`].
///
/// It runs the performer's steps itself (with [`Performer::step`], on the same fixed schedule `Performer::advance`
/// keeps), so it sees every step's frame, and its step count never rewinds when the performer is reset.
///
/// Its **timeline** counts ticks of 1/[`STEP_HZ`] s from the first `advance`, including the ticks a slow loop skips
/// ([`skipped_steps`](Self::skipped_steps)). Anything that means elapsed time (the capture's 10 s stall limit) is
/// measured on it, not in steps executed; the frame rate is measured on the frames' own timestamps.
pub struct PerformControl {
    // Field order matters: the receiver drops first, then `_slot` frees the capture reservation (see `SlotGuard`).
    commands: Receiver<Command>,
    _slot: SlotGuard,
    shared: Arc<Shared>,
    capture: NeutralCapture,
    running: Option<Running>,
    /// The `now` of the first `advance`; step `i` is due `i / STEP_HZ` after it.
    origin: Option<f64>,
    next_step: u64,
    skipped: u64,
    live: Live,
    fps: FpsMeter,
    /// The trigger fades, written into the performer's trigger layer before each of its steps.
    triggers: TriggerLayer,
    /// The attached voice, written into the performer's audio layer and cues before each of its steps.
    voice: Option<Box<Voice>>,
    /// The session's head rest from the first tracked frame, for a calibration that has none (#542).
    head_rest: HeadRest,
    /// Where each advance's last step is published for an output (PF-04), if one runs.
    pose_tap: Option<Arc<dyn PoseTap>>,
    /// The acts playing, written into the performer's score layer before each of its steps. Boxed once, with the
    /// channel: it holds [`MAX_ACTS`] acts of a few hundred bytes each.
    score: Box<LiveScore>,
}

/// Where the first-frame head rest stands (#542). A calibration whose head rest has no position (none saved, or one
/// made from a source without depth) measures the head from a level, centred pose, so a performer who sits turned or
/// leans in starts turned, with no shift or lean. Until a calibration says otherwise, the first tracked frame's head
/// becomes the rest instead, as the prototype did. It is a session default, not a calibration: it goes to the
/// retarget only, so [`PerformHost::calibration`] still reports the calibration as set and nothing saves it; the next
/// session takes its own first frame. Any calibration that is set or captured replaces it.
#[derive(Copy, Clone, Debug, PartialEq)]
enum HeadRest {
    /// No tracked frame with a head position yet.
    Waiting,
    /// Seen on the last step, applied before the next.
    Pending(HeadPose),
    /// Applied, or not needed: the calibration in use has a positioned head rest.
    Done,
}

impl PerformControl {
    /// Runs the performer's steps that are due at `now` (at most [`MAX_STEPS_PER_ADVANCE`], then it skips ahead, as
    /// [`Performer::advance`] does), after taking at most [`MAX_COMMANDS_PER_ADVANCE`] requests from the queue.
    /// Returns the last step run, or `None` if none was due. Publishes the status.
    ///
    /// Never blocks, locks, allocates or frees, except inside the retarget's `set_config` when a settings request
    /// arrives, which its contract allows between frames.
    pub fn advance<'p>(&mut self, performer: &'p mut Performer, now: Seconds) -> Option<PerformOutput<'p>> {
        self.finish_capture(performer);
        for _ in 0..MAX_COMMANDS_PER_ADVANCE {
            let Ok(command) = self.commands.try_recv() else { break };
            self.run(command, performer);
        }
        self.rest_head(performer);
        let Some((first, count)) = self.due_steps(now) else {
            self.publish();
            return None;
        };
        self.check_stall();
        let last = first + count - 1;
        for index in first..last {
            let out = self.step_performer(performer, Tick::at_rate(index, STEP_HZ));
            self.observe(&out);
        }
        let out = self.step_performer(performer, Tick::at_rate(last, STEP_HZ));
        self.observe(&out);
        if let Some(tap) = &self.pose_tap {
            tap.publish(out.pose, out.bus.values.arkit(), out.tick.time);
        }
        self.publish();
        Some(out)
    }

    /// Runs one step. The trigger fades and the voice go first, so the layers they write are the ones this step
    /// resolves.
    fn step_performer<'p>(&mut self, performer: &'p mut Performer, tick: Tick) -> PerformOutput<'p> {
        self.triggers.step(STEP_SECS, performer.trigger_layer_mut());
        let rest = *performer.rest();
        let (score, over, pose) = performer.score_mut();
        self.score.step(tick.time, &rest, score, over, pose);
        if let Some(voice) = self.voice.as_deref_mut() {
            if let Some(frame) = voice.reader.poll() {
                voice.lips.take(&frame);
            }
            let rest = *performer.rest();
            let mut cues = Cues::default();
            voice.lips.step(STEP_SECS, &rest, performer.audio_layer_mut(), &mut cues);
            performer.set_cues(cues);
        }
        performer.step(tick)
    }

    /// Makes the next step due at `now`, as the step after the last one run: the time that passed since is dropped, not
    /// caught up. Call it when a loop that stopped calling [`advance`](Self::advance) (a pause) starts again, so the
    /// performer and everything driven by its steps resume where they stopped instead of running the missed steps or
    /// skipping over them. Before the first `advance` it does nothing.
    pub fn rebase(&mut self, now: Seconds) {
        if self.origin.is_some() {
            #[allow(clippy::cast_precision_loss)] // a step count far below 2^52
            let steps = self.next_step as f64;
            self.origin = Some(now.0 - steps / f64::from(STEP_HZ));
        }
    }

    /// Resets the performer and restarts the control's timeline with it: the next [`advance`](Self::advance) runs
    /// tick 0 again. This is the reset to use (after a seek in a recorded track, say). A bare [`Performer::reset`]
    /// clears the stages but leaves the control's tick count running, which is harmless but does not restart the
    /// clock; this call does both together.
    ///
    /// A running neutral capture keeps the frames it holds and its stall clock starts over at the next `advance`;
    /// the tracking state reads `NoFrames` and the frame rate starts over, as after a new source.
    ///
    /// The triggers start over too: whatever expression is shown is gone at once, and no key counts as pressed, so a
    /// held or toggled key must be pressed again to show its expression. The bindings stay. Key presses and releases
    /// still queued from before the reset are dropped with the rest of that timeline; every other queued request
    /// (settings, bindings, calibration) is applied.
    pub fn reset(&mut self, performer: &mut Performer) {
        // At most the queue's length: a producer that keeps refilling it cannot hold the reset.
        for _ in 0..COMMAND_QUEUE_LEN {
            let Ok(command) = self.commands.try_recv() else { break };
            match command {
                Command::Press { .. } | Command::Release(_) => {}
                // An act asked for before the reset belongs to the timeline that ended: it never plays.
                Command::Act { id, .. } => {
                    self.shared.acts_taken.fetch_max(id, Ordering::AcqRel);
                }
                other => self.run(other, performer),
            }
        }
        performer.reset();
        self.triggers.reset();
        self.score.clear();
        // The first-frame head rest belongs to the timeline that ended: the next tracked frame takes it again.
        let set = self.shared.calibration.read();
        if set.head_rest.position.is_none() {
            performer.retarget_mut().set_calibration(set);
            self.head_rest = HeadRest::Waiting;
        }
        self.origin = None;
        self.next_step = 0;
        self.skipped = 0;
        self.fps = FpsMeter::default();
        self.live.tracking = Tracking::NoFrames;
        self.live.fps = 0.0;
        if let Some(run) = self.running.as_mut() {
            run.progress_tick = None;
        }
        self.publish();
    }

    /// Steps dropped because the loop fell more than [`MAX_STEPS_PER_ADVANCE`] behind.
    pub fn skipped_steps(&self) -> u64 {
        self.skipped
    }

    /// The first due step and how many to run, as [`Performer::advance`] counts them.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a non-negative step count far below 2^63
    fn due_steps(&mut self, now: Seconds) -> Option<(u64, u64)> {
        let origin = *self.origin.get_or_insert(now.0);
        let elapsed = (now.0 - origin).max(0.0);
        let due = ((elapsed * f64::from(STEP_HZ) + BOUNDARY_EPS).floor() as u64).saturating_add(1);
        let pending = due.saturating_sub(self.next_step);
        if pending == 0 {
            return None;
        }
        let run = pending.min(u64::from(MAX_STEPS_PER_ADVANCE));
        let first = self.next_step;
        self.skipped += pending - run;
        self.next_step = due;
        Some((first, run))
    }

    fn run(&mut self, command: Command, performer: &mut Performer) {
        match command {
            Command::Calibrate(frames) => {
                let frames = frames.clamp(MIN_NEUTRAL_FRAMES, MAX_NEUTRAL_FRAMES);
                self.capture.reset();
                self.running = Some(Running { frames, held: 0, progress_tick: None, full: false });
                self.live.calibration = CalibrationState::Capturing { frames: 0, of: frames };
            }
            Command::Config { seq, config } => {
                performer.retarget_mut().set_config(config);
                self.shared.acknowledged.store(seq, Ordering::Release);
            }
            Command::Calibration(calibration) => {
                self.set_calibration(performer, &calibration);
                self.shared.calibrations_applied.fetch_add(1, Ordering::AcqRel);
            }
            Command::Trigger { index, slot, generation } => self.set_trigger(index, slot.as_ref(), generation),
            Command::Triggers(slots, lips) => {
                self.triggers.rebake(&slots);
                if let (Some(voice), Some(lips)) = (self.voice.as_deref_mut(), lips) {
                    voice.lips = *lips;
                }
            }
            Command::Voice(voice) => {
                if voice.is_none() {
                    performer.audio_layer_mut().clear();
                    performer.set_cues(Cues::default());
                }
                self.voice = voice;
            }
            Command::BindBatch(batch) => {
                for (index, slot, generation) in batch.binds() {
                    self.set_trigger(index, slot, generation);
                }
            }
            Command::Press { index, down } => {
                if down {
                    self.triggers.press(index);
                } else {
                    self.triggers.release(index);
                }
            }
            // The caller holds the tap too (see `set_pose_tap`), so the one replaced here is not freed here.
            Command::PoseTap(tap) => self.pose_tap = tap,
            Command::Act { id, act } => {
                self.score.start(id, act);
                self.shared.acts_taken.fetch_max(id, Ordering::AcqRel);
            }
            Command::Release(id) => {
                self.score.release(id);
            }
            Command::Drive(gate) => performer.set_drive(gate),
            Command::Body(style) => {
                performer.animator_mut().set_style(&style);
            }
            Command::React(impact) => {
                performer.animator_mut().react(&impact);
            }
        }
    }

    /// Sets one trigger slot from its baked form. The control side bakes only non-empty masks and in-range indices, so
    /// the table refuses nothing it is sent.
    fn set_trigger(&mut self, index: usize, slot: Option<&TriggerSlot>, generation: u64) {
        let result = self.triggers.set(index, slot, generation);
        debug_assert_eq!(result, Ok(()), "the control side sent a slot the table refuses");
    }

    /// Sets the calibration and publishes it: the mailbox always holds the one in use.
    fn set_calibration(&mut self, performer: &mut Performer, calibration: &Calibration) {
        performer.retarget_mut().set_calibration(*calibration);
        self.shared.calibration.write(calibration);
        self.head_rest = if calibration.head_rest.position.is_some() { HeadRest::Done } else { HeadRest::Waiting };
    }

    /// Applies a first-frame head rest seen on the last step (see [`HeadRest`]), if the calibration in use still has
    /// no positioned one.
    fn rest_head(&mut self, performer: &mut Performer) {
        let HeadRest::Pending(head) = self.head_rest else { return };
        self.head_rest = HeadRest::Done;
        let mut calibration = *performer.retarget().calibration();
        if calibration.head_rest.position.is_none() {
            calibration.head_rest = head;
            performer.retarget_mut().set_calibration(calibration);
        }
    }

    /// Notes the first tracked frame with a positioned head while the first-frame head rest is waiting and no capture
    /// is running (a capture sets the rest itself).
    fn watch_head(&mut self, out: &PerformOutput<'_>) {
        if self.head_rest != HeadRest::Waiting || self.running.is_some() {
            return;
        }
        let tracked = out.frame.filter(|frame| frame.status == TrackStatus::Tracking);
        if let Some(head) = tracked.and_then(|frame| frame.head).filter(|head| head.position.is_some()) {
            self.head_rest = HeadRest::Pending(head);
        }
    }

    /// Takes in one executed step: the tracking state, the frame rate and the capture.
    fn observe(&mut self, out: &PerformOutput<'_>) {
        self.live.tracking = out.tracking;
        if let Some(frame) = out.frame {
            // The frame's own timestamp, not the loop's: the rate is the tracker's, whatever the loop does.
            self.fps.arrived(frame.time.0);
        }
        self.live.fps = self.fps.fps;
        self.watch_head(out);
        self.feed_capture(out);
    }

    /// Pushes the step's tracked frame into the capture and watches for a stall.
    fn feed_capture(&mut self, out: &PerformOutput<'_>) {
        let timeline_end = self.next_step;
        let Some(run) = self.running.as_mut().filter(|run| !run.full) else {
            return;
        };
        let (mut held, _) = self.capture.progress();
        if let Some(frame) = out.frame.filter(|_| held < run.frames) {
            self.capture.push(frame);
            held = self.capture.progress().0;
        }
        self.live.calibration = CalibrationState::Capturing { frames: held, of: run.frames };
        if held >= run.frames {
            run.full = true;
        } else if held > run.held {
            // Progress counts from the end of this call's timeline, not the executed step's own tick: when the loop
            // fell behind, the steps run are the oldest due ones and the timeline is already past them.
            (run.held, run.progress_tick) = (held, Some(timeline_end));
        }
    }

    /// Fails a capture the face has been missing from for [`CAPTURE_STALL_STEPS`] ticks of the control's timeline.
    /// The timeline is the one `due_steps` keeps, which counts the ticks it skips too, so a slow loop or a pause is
    /// 10 s of waiting just as 600 stepped ticks are. Checked before the due steps run, so frames that arrive after a
    /// long gap do not rescue the capture.
    fn check_stall(&mut self) {
        let Some(run) = self.running.as_mut().filter(|run| !run.full) else {
            return;
        };
        // `next_step` is the timeline's end after this call's catch-up (`due_steps` has moved it), so a capture asked
        // for during a pause starts here, with its whole window ahead of it.
        let anchor = *run.progress_tick.get_or_insert(self.next_step);
        if self.next_step.saturating_sub(anchor) >= CAPTURE_STALL_STEPS {
            let (got, _) = self.capture.progress();
            self.live.calibration = CalibrationState::Failed { got, need: run.frames };
            self.running = None;
            self.shared.capturing.store(false, Ordering::Release);
        }
    }

    /// Takes the median and sets the calibration. It waits for the next call because the output of the call that
    /// completed the capture still borrows the performer.
    fn finish_capture(&mut self, performer: &mut Performer) {
        let Some(run) = self.running.take_if(|run| run.full) else {
            return;
        };
        self.live.calibration = match self.capture.finish(performer.retarget().calibration()) {
            Ok(calibration) => {
                self.set_calibration(performer, &calibration);
                CalibrationState::Done { frames: run.frames }
            }
            Err(NeutralError::TooFewFrames { got, need }) => CalibrationState::Failed { got, need },
        };
        self.shared.capturing.store(false, Ordering::Release);
    }

    fn publish(&self) {
        self.shared.live.store(self.live.pack(), Ordering::Release);
        for (published, generation) in self.shared.active_triggers.iter().zip(self.triggers.active_generations()) {
            published.store(generation, Ordering::Release);
        }
        for (published, id) in self.shared.active_acts.iter().zip(self.score.ids()) {
            published.store(id, Ordering::Release);
        }
    }
}

impl std::fmt::Debug for PerformControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerformControl").field("calibration", &self.live.calibration).finish_non_exhaustive()
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use omuzaki_anim::life::Life;
    use omuzaki_anim::retarget::ArkitRetarget;
    use omuzaki_anim_api::{ChannelBuf, HumanoidPose, LifeConfig};
    use omuzaki_anim_mock::animator::MockAnimator;
    use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};

    use super::*;
    use crate::perform::PerformerParts;

    #[test]
    fn the_web_build_queues_but_refuses_to_wait_on_the_performer() {
        let parts = PerformerParts {
            source: Box::new(TrackReplay::new(synthetic_track(&SyntheticSpec::default()))),
            retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
            life: Box::new(Life::new(LifeConfig::new(7))),
            animator: Box::new(MockAnimator),
        };
        let performer = Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST);
        let (host, _control) = perform_channel(&performer);
        let receipt = host.set_retarget(RetargetChange::default()).unwrap();
        assert_eq!(host.await_retarget(receipt.seq, Duration::ZERO), Err(PerformError::Unsupported));
    }
}
