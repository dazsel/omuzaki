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
//! - a finished calibration goes back through a second preallocated queue.
//!
//! The one exception is the retarget's own `set_config`, which its contract allows to allocate between frames.
//! Everything that needs the performer's real settings (the handle's merge base) is reconciled once, in
//! [`PerformControl::sync`], which is setup work and may lock and allocate.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_anim::{MAX_NEUTRAL_FRAMES, MIN_NEUTRAL_FRAMES, NeutralCapture, NeutralError};
use omuzaki_anim_api::{Calibration, DeviceProfile, RetargetConfig, Tick};
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{MAX_STEPS_PER_ADVANCE, PerformOutput, Performer, STEP_HZ, Tracking};

/// Commands the queue holds before [`PerformError::QueueFull`].
pub const COMMAND_QUEUE_LEN: usize = 16;
/// Commands one [`PerformControl::advance`] takes from the queue. A producer can refill the queue as fast as it is
/// drained, so this bounds the work before the performer steps; the rest wait for the next call.
pub const MAX_COMMANDS_PER_ADVANCE: usize = 4;
/// Steps a neutral capture waits for its next tracked frame before it gives up (10 s at the 60 Hz step).
pub const CAPTURE_STALL_STEPS: u64 = 600;
/// Slack so a `now` that lands exactly on a step boundary is not lost to float rounding (as [`Performer::advance`]).
const BOUNDARY_EPS: f64 = 1e-9;
/// Weight of the newest arrival gap in the [`PerformStatus::source_fps`] average.
const FPS_SMOOTHING: f32 = 0.1;

/// What the ops (and the UI) can ask of the live performer. Requests are queued for the perform thread, so a reply
/// means "accepted", not "applied"; [`status`](Self::status) shows what has happened since.
pub trait PerformHost: Send + Sync {
    /// Starts a neutral capture of `frames` tracked frames (clamped to the capture's own limits); the result applies
    /// itself when done.
    ///
    /// # Errors
    /// [`PerformError::CalibrationRunning`] while a capture is running, [`PerformError::QueueFull`] or
    /// [`PerformError::Disconnected`].
    fn calibrate(&self, frames: usize) -> Result<(), PerformError>;

    /// Applies a partial change; returns the full config now in use (as the change will leave it). The result is built
    /// from the last settings this host returned, starting from the performer's own at [`PerformControl::sync`], so a
    /// change made around the host, directly on the performer's retarget, is not seen.
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`].
    fn set_retarget(&self, change: RetargetChange) -> Result<RetargetConfig, PerformError>;

    /// Replaces the calibration in use (a saved profile loading, say).
    ///
    /// # Errors
    /// [`PerformError::QueueFull`] or [`PerformError::Disconnected`].
    fn set_calibration(&self, calibration: Calibration) -> Result<(), PerformError>;

    /// The calibration in use (from the last published status).
    fn calibration(&self) -> Calibration;

    /// The status the perform thread published last.
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
    /// The last capture gave up: the face was not seen for [`CAPTURE_STALL_STEPS`] steps.
    Failed {
        /// Tracked frames held when it gave up.
        got: usize,
        /// Tracked frames wanted.
        need: usize,
    },
}

/// What the perform thread last published. One value is always internally consistent: it is copied under one lock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PerformStatus {
    /// Whether a face is tracked.
    pub tracking: Tracking,
    /// The neutral capture.
    pub calibration: CalibrationState,
    /// The settings in use.
    pub retarget: RetargetConfig,
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
    /// A change made before [`PerformControl::sync`]: the handle did not yet know the performer's own settings, so the
    /// change is merged into them on the perform side (once, at setup).
    Retarget(RetargetChange),
    /// The full config the handle built after sync.
    Config(RetargetConfig),
    Calibration(Calibration),
}

/// What the handle knows of the performer, behind one lock that only the handle side takes (and
/// [`PerformControl::sync`], once).
struct HandleState {
    /// The settings as they will be once every queued request has run: the base of the next merge.
    config: RetargetConfig,
    /// The calibration the performer last reported.
    calibration: Calibration,
    /// Whether `config` and `calibration` are the performer's own (set by [`PerformControl::sync`]).
    synced: bool,
    /// Calibrations the performer reports, oldest first.
    reports: Receiver<Calibration>,
}

impl HandleState {
    /// Takes in what the performer reported since the last call.
    fn drain_reports(&mut self) {
        while let Ok(calibration) = self.reports.try_recv() {
            self.calibration = calibration;
        }
    }
}

/// The record both ends share.
struct Shared {
    /// Tracking, capture state and frame rate in one word (see [`Live`]), so a read is always consistent.
    live: AtomicU64,
    /// Set by the handle when it queues a capture, cleared by the control when the capture ends or the control drops.
    capturing: AtomicBool,
    state: Mutex<HandleState>,
}

/// Locks `m`; a panic elsewhere cannot leave these plain records half-written, so a poisoned lock is still good.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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

/// Frees the capture slot when dropped. A field of [`PerformControl`] declared after its receiver, so by the time it
/// runs the queue is already disconnected: a `calibrate` that reserved the slot meanwhile finds its send refused and
/// releases the slot itself.
struct SlotGuard(Arc<Shared>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.capturing.store(false, Ordering::Release);
    }
}

/// The two ends of the performer's control channel: hand the [`PerformHandle`] to the ops and keep the
/// [`PerformControl`] on the perform thread.
pub fn perform_channel() -> (PerformHandle, PerformControl) {
    let (commands_tx, commands_rx) = sync_channel(COMMAND_QUEUE_LEN);
    // Room for every request in the queue plus a finished capture, doubled: the handle drains it before each send.
    let (reports_tx, reports_rx) = sync_channel(2 * COMMAND_QUEUE_LEN);
    let shared = Arc::new(Shared {
        live: AtomicU64::new(Live::START.pack()),
        capturing: AtomicBool::new(false),
        state: Mutex::new(HandleState {
            config: RetargetConfig::default(),
            calibration: Calibration::identity(DeviceProfile::Webcam),
            synced: false,
            reports: reports_rx,
        }),
    });
    let control = PerformControl {
        commands: commands_rx,
        _slot: SlotGuard(Arc::clone(&shared)),
        shared: Arc::clone(&shared),
        reports: reports_tx,
        // The capture's buffers are sized for the longest capture, once; a shorter one just stops pushing earlier.
        capture: NeutralCapture::new(MAX_NEUTRAL_FRAMES),
        running: None,
        synced: false,
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

    /// The state with the performer's latest reports taken in.
    fn state(&self) -> MutexGuard<'_, HandleState> {
        let mut state = lock(&self.shared.state);
        state.drain_reports();
        state
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

    fn set_retarget(&self, change: RetargetChange) -> Result<RetargetConfig, PerformError> {
        // Held across the send, so changes reach the performer in the order they are merged here, and so a sync
        // cannot slip between choosing the command's form and queueing it.
        let mut state = self.state();
        let mut config = state.config.clone();
        change.apply_to(&mut config);
        let command = if state.synced { Command::Config(config.clone()) } else { Command::Retarget(change) };
        self.send(command)?;
        state.config.clone_from(&config);
        Ok(config)
    }

    fn set_calibration(&self, calibration: Calibration) -> Result<(), PerformError> {
        // Drain first: the performer reports every calibration it sets, and the report queue must have room.
        drop(self.state());
        self.send(Command::Calibration(calibration))
    }

    fn calibration(&self) -> Calibration {
        self.state().calibration
    }

    fn status(&self) -> PerformStatus {
        let live = Live::unpack(self.shared.live.load(Ordering::Acquire));
        let retarget = self.state().config.clone();
        PerformStatus { tracking: live.tracking, calibration: live.calibration, retarget, source_fps: live.fps }
    }
}

/// A capture in progress.
struct Running {
    /// Tracked frames wanted.
    frames: usize,
    /// Tracked frames held at the last step that made progress.
    held: usize,
    /// Steps since the capture last made progress, counted by the control itself so a performer reset cannot confuse it.
    idle: u64,
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
pub struct PerformControl {
    // Field order matters: the receiver drops first, then `_slot` frees the capture reservation (see `SlotGuard`).
    commands: Receiver<Command>,
    _slot: SlotGuard,
    shared: Arc<Shared>,
    reports: SyncSender<Calibration>,
    capture: NeutralCapture,
    running: Option<Running>,
    synced: bool,
    /// The `now` of the first `advance`; step `i` is due `i / STEP_HZ` after it.
    origin: Option<f64>,
    next_step: u64,
    skipped: u64,
    live: Live,
    fps: FpsMeter,
}

impl PerformControl {
    /// Hands the handle the performer's own settings and calibration, and runs the requests queued before now.
    ///
    /// This is setup work: it locks, allocates and is not bounded by [`MAX_COMMANDS_PER_ADVANCE`]. [`advance`] calls
    /// it on its first call; call it yourself at startup to keep that out of the loop. It does nothing after the
    /// first time.
    ///
    /// [`advance`]: Self::advance
    pub fn sync(&mut self, performer: &mut Performer) {
        if self.synced {
            return;
        }
        self.synced = true;
        // The handle's lock makes this atomic with its `set_retarget`: a change queued before this point is run here,
        // and one made after it sees `synced` and carries a full config.
        let shared = Arc::clone(&self.shared);
        let mut state = lock(&shared.state);
        while let Ok(command) = self.commands.try_recv() {
            self.run(command, performer);
        }
        state.config.clone_from(performer.retarget().config());
        state.calibration = *performer.retarget().calibration();
        state.synced = true;
    }

    /// Runs the performer's steps that are due at `now` (at most [`MAX_STEPS_PER_ADVANCE`], then it skips ahead, as
    /// [`Performer::advance`] does), after taking at most [`MAX_COMMANDS_PER_ADVANCE`] requests from the queue.
    /// Returns the last step run, or `None` if none was due. Publishes the status.
    ///
    /// Never blocks. After the first call it takes no lock and neither allocates nor frees, except inside the
    /// retarget's `set_config` when a settings request arrives, which its contract allows between frames.
    pub fn advance<'p>(&mut self, performer: &'p mut Performer, now: Seconds) -> Option<PerformOutput<'p>> {
        self.sync(performer);
        self.finish_capture(performer);
        for _ in 0..MAX_COMMANDS_PER_ADVANCE {
            let Ok(command) = self.commands.try_recv() else { break };
            self.run(command, performer);
        }
        let Some((first, count)) = self.due_steps(now) else {
            self.publish();
            return None;
        };
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
                self.running = Some(Running { frames, held: 0, idle: 0, full: false });
                self.live.calibration = CalibrationState::Capturing { frames: 0, of: frames };
            }
            Command::Retarget(change) => {
                let mut config = performer.retarget().config().clone();
                change.apply_to(&mut config);
                performer.retarget_mut().set_config(config);
            }
            Command::Config(config) => performer.retarget_mut().set_config(config),
            Command::Calibration(calibration) => self.set_calibration(performer, &calibration),
        }
    }

    fn set_calibration(&mut self, performer: &mut Performer, calibration: &Calibration) {
        performer.retarget_mut().set_calibration(*calibration);
        // The queue is preallocated and sized for every request that can be outstanding (the handle drains it before
        // each send), so a full queue is a bug, not load.
        if self.reports.try_send(*calibration).is_err() {
            debug_assert!(false, "the calibration report queue is full");
        }
    }

    /// Takes in one executed step: the tracking state, the frame rate and the capture.
    fn observe(&mut self, out: &PerformOutput<'_>) {
        self.live.tracking = out.tracking;
        if out.frame.is_some() {
            self.fps.arrived(out.tick.time.0);
        }
        self.live.fps = self.fps.fps;
        self.feed_capture(out);
    }

    /// Pushes the step's tracked frame into the capture and watches for a stall.
    fn feed_capture(&mut self, out: &PerformOutput<'_>) {
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
            (run.held, run.idle) = (held, 0);
        } else {
            run.idle += 1;
            if run.idle >= CAPTURE_STALL_STEPS {
                self.live.calibration = CalibrationState::Failed { got: held, need: run.frames };
                self.running = None;
                self.shared.capturing.store(false, Ordering::Release);
            }
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
