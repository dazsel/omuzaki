//! [`PerformHost`]: what the ops (and the UI) can ask of the live performer, across the thread boundary.
//!
//! The [`Performer`] lives on the perform thread and ops run elsewhere, so they never touch it. [`perform_channel`]
//! makes the two ends: a [`PerformHandle`] (implements [`PerformHost`]; the App injects it as `Arc<dyn PerformHost>`)
//! and a [`PerformControl`] the perform thread calls instead of [`Performer::advance`]. Commands go one way through a
//! bounded queue; the status comes back through a small shared record that the perform thread only ever `try_lock`s.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use omuzaki_anim::{MAX_NEUTRAL_FRAMES, MIN_NEUTRAL_FRAMES, NeutralCapture, NeutralError};
use omuzaki_anim_api::{Calibration, DeviceProfile, RetargetConfig};
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{PerformOutput, Performer, Tracking};

/// Commands the queue holds before [`PerformError::QueueFull`].
pub const COMMAND_QUEUE_LEN: usize = 16;
/// Steps a neutral capture waits for its next tracked frame before it gives up (10 s at the 60 Hz step).
pub const CAPTURE_STALL_STEPS: u64 = 600;
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
    /// from the last settings this host returned (or published), so a change made around the host, directly on the
    /// performer's retarget, shows in it only after the next published status.
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

enum Command {
    Calibrate(usize),
    /// A change, not a full config: the perform thread merges it into the config actually in use.
    Retarget(RetargetChange),
    /// Boxed: a calibration is large next to the other commands, and this one is rare.
    Calibration(Box<Calibration>),
}

/// The record both ends share. The perform side only `try_lock`s it.
struct Shared {
    published: Mutex<Published>,
    /// Set by the handle when it queues a capture, cleared by the control when the capture ends.
    capturing: AtomicBool,
    /// The config the handle last returned, the base for the next merge. Handle side only.
    last_config: Mutex<Option<RetargetConfig>>,
}

struct Published {
    status: PerformStatus,
    calibration: Calibration,
}

/// Locks `m`; a panic elsewhere cannot leave these plain records half-written, so a poisoned lock is still good.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The ops' side. Cheap to clone; every clone talks to the same performer.
#[derive(Clone)]
pub struct PerformHandle {
    commands: SyncSender<Command>,
    shared: Arc<Shared>,
}

/// The two ends of the performer's control channel: hand the [`PerformHandle`] to the ops and keep the
/// [`PerformControl`] on the perform thread.
pub fn perform_channel() -> (PerformHandle, PerformControl) {
    let (tx, rx) = sync_channel(COMMAND_QUEUE_LEN);
    let shared = Arc::new(Shared {
        published: Mutex::new(Published {
            status: PerformStatus {
                tracking: Tracking::NoFrames,
                calibration: CalibrationState::Idle,
                retarget: RetargetConfig::default(),
                source_fps: 0.0,
            },
            calibration: Calibration::identity(DeviceProfile::Webcam),
        }),
        capturing: AtomicBool::new(false),
        last_config: Mutex::new(None),
    });
    let control = PerformControl {
        commands: rx,
        shared: Arc::clone(&shared),
        // The capture's buffers are sized for the longest capture, once; a shorter one just stops pushing earlier.
        capture: NeutralCapture::new(MAX_NEUTRAL_FRAMES),
        running: None,
        synced: false,
        pending_config: None,
        pending_calibration: None,
        tracking: Tracking::NoFrames,
        calibration_state: CalibrationState::Idle,
        fps: FpsMeter::default(),
    };
    (PerformHandle { commands: tx, shared }, control)
}

impl PerformHandle {
    fn send(&self, command: Command) -> Result<(), PerformError> {
        self.commands.try_send(command).map_err(|e| match e {
            TrySendError::Full(_) => PerformError::QueueFull,
            TrySendError::Disconnected(_) => PerformError::Disconnected,
        })
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
        // Held across the send, so two callers' changes reach the performer in the order they are merged here.
        let mut last = lock(&self.shared.last_config);
        let mut config = last.take().unwrap_or_else(|| lock(&self.shared.published).status.retarget.clone());
        if let Err(e) = self.send(Command::Retarget(change)) {
            *last = Some(config);
            return Err(e);
        }
        change.apply_to(&mut config);
        *last = Some(config.clone());
        Ok(config)
    }

    fn set_calibration(&self, calibration: Calibration) -> Result<(), PerformError> {
        self.send(Command::Calibration(Box::new(calibration)))
    }

    fn calibration(&self) -> Calibration {
        lock(&self.shared.published).calibration
    }

    fn status(&self) -> PerformStatus {
        lock(&self.shared.published).status.clone()
    }
}

/// A capture in progress.
struct Running {
    /// Tracked frames wanted.
    frames: usize,
    /// Tracked frames held when the stall clock last restarted.
    held: usize,
    /// The step the capture last made progress on (or started at): the stall clock.
    anchor: Option<u64>,
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
pub struct PerformControl {
    commands: Receiver<Command>,
    shared: Arc<Shared>,
    capture: NeutralCapture,
    running: Option<Running>,
    /// Whether the performer's own config and calibration have been published once.
    synced: bool,
    /// Values waiting for a free lock. Set only when something changed, so a quiet loop clones nothing.
    pending_config: Option<RetargetConfig>,
    pending_calibration: Option<Calibration>,
    tracking: Tracking,
    calibration_state: CalibrationState,
    fps: FpsMeter,
}

impl PerformControl {
    /// Drains pending commands (`try_recv`), feeds the neutral capture, runs `performer.advance(now)` and publishes the
    /// status. Never blocks. It allocates only on the first call (publishing the performer's own settings) and when a
    /// command arrives (a settings change compiles the retarget's tables, which its docs allow between frames).
    ///
    /// The capture sees the frame of the last step each call runs; on a call that catches up several steps the earlier
    /// steps' frames are skipped, which only makes the capture a little slower.
    pub fn advance<'p>(&mut self, performer: &'p mut Performer, now: Seconds) -> Option<PerformOutput<'p>> {
        self.finish_capture(performer);
        if !self.synced {
            self.synced = true;
            self.pending_config = Some(performer.retarget().config().clone());
            self.pending_calibration = Some(*performer.retarget().calibration());
        }
        while let Ok(command) = self.commands.try_recv() {
            self.run(&command, performer);
        }
        let output = performer.advance(now);
        if let Some(out) = &output {
            self.tracking = out.tracking;
            if out.frame.is_some() {
                self.fps.arrived(out.tick.time.0);
            }
            self.feed_capture(out);
        }
        self.publish();
        output
    }

    fn run(&mut self, command: &Command, performer: &mut Performer) {
        match command {
            Command::Calibrate(frames) => {
                let frames = (*frames).clamp(MIN_NEUTRAL_FRAMES, MAX_NEUTRAL_FRAMES);
                self.capture.reset();
                self.running = Some(Running { frames, held: 0, anchor: None, full: false });
                self.calibration_state = CalibrationState::Capturing { frames: 0, of: frames };
            }
            Command::Retarget(change) => {
                let mut config = performer.retarget().config().clone();
                (*change).apply_to(&mut config);
                performer.retarget_mut().set_config(config.clone());
                self.pending_config = Some(config);
            }
            Command::Calibration(calibration) => {
                performer.retarget_mut().set_calibration(**calibration);
                self.pending_calibration = Some(**calibration);
            }
        }
    }

    /// Pushes the step's tracked frame into the capture and watches for a stall.
    fn feed_capture(&mut self, out: &PerformOutput<'_>) {
        let Some(run) = self.running.as_mut().filter(|run| !run.full) else {
            return;
        };
        let index = out.tick.index;
        let anchor = *run.anchor.get_or_insert(index);
        let (mut held, _) = self.capture.progress();
        if let Some(frame) = out.frame.filter(|_| held < run.frames) {
            self.capture.push(frame);
            held = self.capture.progress().0;
        }
        self.calibration_state = CalibrationState::Capturing { frames: held, of: run.frames };
        if held >= run.frames {
            run.full = true;
        } else if held > run.held {
            (run.held, run.anchor) = (held, Some(index));
        } else if index.saturating_sub(anchor) + 1 >= CAPTURE_STALL_STEPS {
            // `saturating_sub`: a performer `reset` restarts the tick index below the anchor.
            self.calibration_state = CalibrationState::Failed { got: held, need: run.frames };
            self.running = None;
            self.shared.capturing.store(false, Ordering::Release);
        }
    }

    /// Takes the median and sets the calibration. It waits for the call after the last frame because the output of
    /// that call borrows the performer.
    fn finish_capture(&mut self, performer: &mut Performer) {
        let Some(run) = self.running.take_if(|run| run.full) else {
            return;
        };
        self.calibration_state = match self.capture.finish(performer.retarget().calibration()) {
            Ok(calibration) => {
                performer.retarget_mut().set_calibration(calibration);
                self.pending_calibration = Some(calibration);
                CalibrationState::Done { frames: run.frames }
            }
            Err(NeutralError::TooFewFrames { got, need }) => CalibrationState::Failed { got, need },
        };
        self.shared.capturing.store(false, Ordering::Release);
    }

    fn publish(&mut self) {
        let mut published = match self.shared.published.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        published.status.tracking = self.tracking;
        published.status.calibration = self.calibration_state;
        published.status.source_fps = self.fps.fps;
        if let Some(config) = self.pending_config.take() {
            published.status.retarget = config;
        }
        if let Some(calibration) = self.pending_calibration.take() {
            published.calibration = calibration;
        }
    }
}

impl Drop for PerformControl {
    /// A capture can never finish now: free the slot so the handle reports `Disconnected`, not `CalibrationRunning`.
    fn drop(&mut self) {
        self.shared.capturing.store(false, Ordering::Release);
    }
}

impl std::fmt::Debug for PerformControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerformControl").field("calibration", &self.calibration_state).finish_non_exhaustive()
    }
}
