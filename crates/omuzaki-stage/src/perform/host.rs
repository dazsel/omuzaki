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
//! # What is reported
//!
//! The handle keeps two views of the settings. **Requested** is the base of the next merge, so back-to-back changes
//! compose; it is also what [`PerformHost::set_retarget`] answers with, labelled as requested and numbered
//! ([`RetargetReceipt`]). **Applied** is what the performer has acknowledged, and it is the only view
//! [`PerformHost::status`] reports, with the number of the newest request it includes. The handle is made from the
//! performer ([`perform_channel`]), so both views start from the settings really in use.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use omuzaki_anim::{MAX_NEUTRAL_FRAMES, MIN_NEUTRAL_FRAMES, NeutralCapture, NeutralError};
use omuzaki_anim_api::{ARKIT_COUNT, Calibration, Channel, DeviceProfile, HeadPose, RetargetConfig, Tick};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

    /// The status the perform thread published last. Reports applied state only. Its parts are read one after the
    /// other, so they can describe slightly different instants (see [`PerformStatus`]).
    fn status(&self) -> PerformStatus;
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
    /// Handle side only; the control never touches it.
    book: Mutex<RetargetBook>,
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
        book: Mutex::new(RetargetBook {
            requested: config.clone(),
            applied: config,
            applied_seq: 0,
            last_seq: 0,
            in_flight: VecDeque::with_capacity(COMMAND_QUEUE_LEN),
        }),
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
        self.send(Command::Calibration(calibration))
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
        let Some((first, count)) = self.due_steps(now) else {
            self.publish();
            return None;
        };
        self.check_stall();
        let last = first + count - 1;
        for index in first..last {
            let out = performer.step(Tick::at_rate(index, STEP_HZ));
            self.observe(&out);
        }
        let out = performer.step(Tick::at_rate(last, STEP_HZ));
        self.observe(&out);
        self.publish();
        Some(out)
    }

    /// Resets the performer and restarts the control's timeline with it: the next [`advance`](Self::advance) runs
    /// tick 0 again. This is the reset to use (after a seek in a recorded track, say). A bare [`Performer::reset`]
    /// clears the stages but leaves the control's tick count running, which is harmless but does not restart the
    /// clock; this call does both together.
    ///
    /// A running neutral capture keeps the frames it holds and its stall clock starts over at the next `advance`;
    /// the tracking state reads `NoFrames` and the frame rate starts over, as after a new source.
    pub fn reset(&mut self, performer: &mut Performer) {
        performer.reset();
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
            Command::Calibration(calibration) => self.set_calibration(performer, &calibration),
        }
    }

    /// Sets the calibration and publishes it: the mailbox always holds the one in use.
    fn set_calibration(&self, performer: &mut Performer, calibration: &Calibration) {
        performer.retarget_mut().set_calibration(*calibration);
        self.shared.calibration.write(calibration);
    }

    /// Takes in one executed step: the tracking state, the frame rate and the capture.
    fn observe(&mut self, out: &PerformOutput<'_>) {
        self.live.tracking = out.tracking;
        if let Some(frame) = out.frame {
            // The frame's own timestamp, not the loop's: the rate is the tracker's, whatever the loop does.
            self.fps.arrived(frame.time.0);
        }
        self.live.fps = self.fps.fps;
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
