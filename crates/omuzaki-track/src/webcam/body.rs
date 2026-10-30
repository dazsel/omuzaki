//! Body tracking beside the face (PF-06, PF-07): a tap that hands the camera thread's newest RGB frame to a body thread,
//! and that thread, which runs the [`BodyTracker`] and publishes to the stage's [`BodyBoard`].
//!
//! The face keeps its own budget: the camera thread only copies the frame into the tap after it has published the
//! face, and skips the copy when the body thread is holding the slot. The body thread runs at whatever rate its models
//! allow and always takes the newest frame, so a slow body never queues frames or delays the face.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::Seconds;
use omuzaki_track_api::body::{BodyBoard, BodyConfig, BodyFrame, BodyState, BodyStatus, BodyWriter, HandGestures};
use omuzaki_track_api::error::TrackError;

use crate::body::BodyTracker;
use crate::body::gesture::HandGestureDetector;
use crate::body::models::PoseSize;
use crate::frame::RgbFrame;

/// Builds the body tracker for a pose size: the App's way to its models (cold, seconds; called on the body thread).
pub type BodyLoader = Arc<dyn Fn(PoseSize) -> Result<BodyTracker, String> + Send + Sync>;

/// How long the body thread waits for a frame before it checks whether to stop.
const WAIT: Duration = Duration::from_millis(100);
/// A gap this long between frames is a new session: the tracker starts over instead of tracking from stale regions.
const NEW_SESSION_S: f64 = 0.5;
/// How long the body thread waits for the last tracker to hand the board's writer back.
const LEASE_WAIT: Duration = Duration::from_secs(5);
/// How long switching off waits for the thread before letting it go.
const JOIN_GRACE: Duration = Duration::from_secs(2);
/// Frame times kept for the percentiles.
const TIMES: usize = 64;

/// One frame in the tap: pixels and when they were captured.
#[derive(Default)]
struct TapFrame {
    rgb: Vec<u8>,
    width: u32,
    height: u32,
    time: Seconds,
}

#[derive(Default)]
struct TapSlot {
    frame: TapFrame,
    fresh: bool,
}

/// The camera thread's hand-off to the body thread: newest wins.
#[derive(Default)]
pub(super) struct BodyTap {
    on: AtomicBool,
    slot: Mutex<TapSlot>,
    ready: Condvar,
}

impl BodyTap {
    pub(super) fn set(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    /// Camera thread: copies `rgb` into the slot, unless the tap is off or the body thread holds the slot right now
    /// (that frame is skipped, never waited for). Allocates only when the frame size changes.
    pub(super) fn offer(&self, rgb: &RgbFrame<'_>, time: Seconds) {
        if !self.on.load(Ordering::Relaxed) || rgb.validate().is_err() {
            return;
        }
        let Ok(mut slot) = self.slot.try_lock() else { return };
        let row = 3 * rgb.width as usize;
        let frame = &mut slot.frame;
        frame.rgb.clear();
        for y in 0..rgb.height as usize {
            frame.rgb.extend_from_slice(&rgb.data[y * rgb.stride..y * rgb.stride + row]);
        }
        (frame.width, frame.height, frame.time) = (rgb.width, rgb.height, time);
        slot.fresh = true;
        drop(slot);
        self.ready.notify_one();
    }

    /// Body thread: swaps the newest frame into `into` once one comes, waiting at most [`WAIT`]. False if none came.
    fn take(&self, into: &mut TapFrame) -> bool {
        let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut slot, _) =
            self.ready.wait_timeout_while(slot, WAIT, |slot| !slot.fresh).unwrap_or_else(PoisonError::into_inner);
        if !slot.fresh {
            return false;
        }
        slot.fresh = false;
        std::mem::swap(&mut slot.frame, into);
        true
    }

    /// Wakes the body thread (to stop).
    fn wake(&self) {
        self.ready.notify_all();
    }
}

/// The arms and hands switches as one word, so a change reaches the running thread without a restart.
const ARMS: u8 = 1;
const HANDS: u8 = 2;

fn wanted_bits(config: BodyConfig) -> u8 {
    u8::from(config.arms) * ARMS | u8::from(config.hands) * HANDS
}

/// What the body thread and the tracker share.
struct Shared {
    stop: AtomicBool,
    wanted: AtomicU8,
    status: Mutex<BodyStatus>,
}

impl Shared {
    fn status(&self) -> std::sync::MutexGuard<'_, BodyStatus> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A running body thread.
pub(super) struct BodyWorker {
    shared: Arc<Shared>,
    size: PoseSize,
    thread: JoinHandle<()>,
}

impl BodyWorker {
    /// Starts a body thread that loads its tracker with `loader` and publishes to `board` whatever `tap` gives it.
    pub(super) fn start(
        config: BodyConfig,
        loader: &BodyLoader,
        tap: &Arc<BodyTap>,
        board: &Arc<BodyBoard>,
    ) -> std::io::Result<Self> {
        let size = if config.precise { PoseSize::Full } else { PoseSize::Lite };
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            wanted: AtomicU8::new(wanted_bits(config)),
            status: Mutex::new(BodyStatus { config, state: BodyState::Loading, ..BodyStatus::default() }),
        });
        let run = Run {
            shared: Arc::clone(&shared),
            loader: Arc::clone(loader),
            tap: Arc::clone(tap),
            board: Arc::clone(board),
            size,
        };
        let thread = std::thread::Builder::new().name("omuzaki-body-track".to_owned()).spawn(move || run.run())?;
        Ok(Self { shared, size, thread })
    }

    /// The pose size its models are.
    pub(super) fn size(&self) -> PoseSize {
        self.size
    }

    /// Changes what it tracks, on the same models.
    pub(super) fn set(&self, config: BodyConfig) {
        self.shared.wanted.store(wanted_bits(config), Ordering::Relaxed);
        self.shared.status().config = config;
    }

    pub(super) fn status(&self) -> BodyStatus {
        self.shared.status().clone()
    }

    /// Stops the thread, waiting a short while for it; a thread still loading its models is let go and ends when they
    /// have loaded.
    pub(super) fn stop(self, tap: &BodyTap) {
        self.shared.stop.store(true, Ordering::Relaxed);
        tap.wake();
        let give_up = Instant::now() + JOIN_GRACE;
        while !self.thread.is_finished() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.thread.is_finished() {
            self.thread.join().ok();
        }
    }
}

/// The body thread's own state.
struct Run {
    shared: Arc<Shared>,
    loader: BodyLoader,
    tap: Arc<BodyTap>,
    board: Arc<BodyBoard>,
    size: PoseSize,
}

impl Run {
    fn run(self) {
        let mut tracker = match (self.loader)(self.size) {
            Ok(tracker) => tracker,
            Err(why) => {
                self.shared.status().state = BodyState::Failed(why);
                return;
            }
        };
        let Some(mut writer) = self.lease() else {
            self.shared.status().state =
                BodyState::Failed("another body tracker still holds the stage's body input".to_owned());
            return;
        };
        self.shared.status().state = BodyState::Running;
        let mut frame = TapFrame::default();
        let mut out = BodyFrame::empty(0, Seconds(0.0));
        let mut gestures = HandGestureDetector::default();
        let (mut seq, mut last_time, mut saw) = (0_u64, None::<f64>, false);
        let mut times = [0.0_f32; TIMES];
        let mut frames = 0_u64;
        while !self.shared.stop.load(Ordering::Relaxed) {
            if !self.tap.take(&mut frame) {
                // No frame: the session stopped or the camera stalled. Say the body is gone once, so the stage eases
                // the arms back instead of holding the last pose.
                if saw {
                    seq += 1;
                    writer.publish(&BodyFrame::empty(seq, last_time.map_or(Seconds(0.0), Seconds)));
                    gestures.reset();
                    writer.show(HandGestures::NONE);
                    saw = false;
                    self.shared.status().arms = [0.0; 2];
                    self.shared.status().hands = [0.0; 2];
                }
                continue;
            }
            let time = frame.time.0;
            if last_time.is_none_or(|last| !(0.0..NEW_SESSION_S).contains(&(time - last))) {
                tracker.reset();
                gestures.reset();
            }
            last_time = Some(time);
            let bits = self.shared.wanted.load(Ordering::Relaxed);
            let wanted = BodyConfig { arms: bits & ARMS != 0, hands: bits & HANDS != 0, precise: false };
            let rgb =
                RgbFrame { data: &frame.rgb, width: frame.width, height: frame.height, stride: 3 * frame.width as usize };
            let started = Instant::now();
            if tracker.track(&rgb, time, &wanted, &mut out).is_err() {
                out = BodyFrame::empty(0, Seconds(time));
            }
            #[allow(clippy::cast_possible_truncation)] // milliseconds of one frame
            let ms = started.elapsed().as_secs_f32() * 1e3;
            seq += 1;
            (out.seq, out.time) = (seq, Seconds(time));
            writer.publish(&out);
            writer.show(gestures.update(&out));
            saw = out.sees_anything();
            #[allow(clippy::cast_possible_truncation)] // a ring index
            {
                times[(frames % TIMES as u64) as usize] = ms;
            }
            frames += 1;
            let (p50, p95) = percentiles(&times, frames);
            let mut status = self.shared.status();
            (status.arms, status.hands, status.frames, status.ms_p50, status.ms_p95) =
                (out.arms, out.hands, frames, p50, p95);
        }
        // Switched off: the stage lets the body go.
        writer.publish(&BodyFrame::empty(seq + 1, last_time.map_or(Seconds(0.0), Seconds)));
        self.shared.status().state = BodyState::Off;
    }

    /// The board's writer, waiting for a tracker that is still letting go of it.
    fn lease(&self) -> Option<BodyWriter> {
        let give_up = Instant::now() + LEASE_WAIT;
        loop {
            if let Some(writer) = self.board.writer() {
                return Some(writer);
            }
            if self.shared.stop.load(Ordering::Relaxed) || Instant::now() >= give_up {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// The median and 95th percentile of the recent frame times (the first `seen` of the ring, at most all of it).
fn percentiles(times: &[f32; TIMES], seen: u64) -> (f32, f32) {
    let n = usize::try_from(seen).map_or(TIMES, |n| n.min(TIMES));
    if n == 0 {
        return (0.0, 0.0);
    }
    let mut sorted = *times;
    let recent = &mut sorted[..n];
    recent.sort_unstable_by(f32::total_cmp);
    (recent[n / 2], recent[(n * 95 / 100).min(n - 1)])
}

/// The webcam tracker's body part: how to load the models, the tap, and the thread while one runs.
pub(super) struct BodyPart {
    pub(super) loader: Option<BodyLoader>,
    pub(super) tap: Arc<BodyTap>,
    worker: Option<BodyWorker>,
    config: BodyConfig,
}

impl Default for BodyPart {
    fn default() -> Self {
        Self { loader: None, tap: Arc::default(), worker: None, config: BodyConfig::OFF }
    }
}

impl BodyPart {
    /// Switches body tracking to `config`: starts the thread, changes what it tracks, restarts it on the other pose
    /// model, or stops it. Returns the config before.
    pub(super) fn set(&mut self, config: BodyConfig, board: &Arc<BodyBoard>) -> Result<BodyConfig, TrackError> {
        let Some(loader) = self.loader.clone() else {
            return Err(TrackError::Unsupported("body tracking: no body models".to_owned()));
        };
        let before = self.config;
        let size = if config.precise { PoseSize::Full } else { PoseSize::Lite };
        // A thread that failed to load is replaced, so asking again retries.
        let usable = |w: &BodyWorker| w.size() == size && !matches!(w.status().state, BodyState::Failed(_) | BodyState::Off);
        match self.worker.take() {
            Some(worker) if config.any() && usable(&worker) => {
                worker.set(config);
                self.worker = Some(worker);
            }
            running => {
                if let Some(worker) = running {
                    worker.stop(&self.tap);
                }
                if config.any() {
                    let worker = BodyWorker::start(config, &loader, &self.tap, board)
                        .map_err(|e| TrackError::Backend(format!("body thread: {e}")))?;
                    self.worker = Some(worker);
                }
            }
        }
        self.tap.set(config.any());
        self.config = config;
        Ok(before)
    }

    pub(super) fn status(&self) -> BodyStatus {
        self.worker.as_ref().map_or_else(BodyStatus::default, BodyWorker::status)
    }

    /// Stops the thread, if any.
    pub(super) fn stop(&mut self) {
        self.tap.set(false);
        if let Some(worker) = self.worker.take() {
            worker.stop(&self.tap);
        }
        self.config = BodyConfig::OFF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tap_hands_over_the_newest_frame_and_only_while_on() {
        let tap = BodyTap::default();
        let pixels = [7_u8; 2 * 2 * 3];
        let frame = RgbFrame { data: &pixels, width: 2, height: 2, stride: 6 };
        let mut into = TapFrame::default();
        tap.offer(&frame, Seconds(1.0));
        assert!(!tap.take(&mut into), "off: nothing is copied");
        tap.set(true);
        tap.offer(&frame, Seconds(1.0));
        let newer = [9_u8; 2 * 2 * 3];
        tap.offer(&RgbFrame { data: &newer, ..frame }, Seconds(2.0));
        assert!(tap.take(&mut into));
        assert_eq!((into.time, into.width, into.rgb.as_slice()), (Seconds(2.0), 2, newer.as_slice()));
        assert!(!tap.take(&mut into), "taken once");
    }

    #[test]
    fn percentiles_read_only_the_frames_seen() {
        let mut times = [0.0; TIMES];
        times[..4].copy_from_slice(&[4.0, 1.0, 3.0, 2.0]);
        assert_eq!(percentiles(&times, 4), (3.0, 4.0));
        assert_eq!(percentiles(&times, 0), (0.0, 0.0));
    }
}
