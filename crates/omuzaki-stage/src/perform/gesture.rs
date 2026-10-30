//! The gesture watch (spec v3 PF-25): face gestures from the running capture press their trigger keys, so a
//! performer who binds `gesture:tongue_out` to "bleh" with `trigger.bind` shows it by sticking their tongue out. Hand
//! gestures (PF-06: a peace sign, a fist, a thumbs up) press theirs the same way while body tracking runs: the body
//! tracker decides them and shows them on the [`BodyBoard`], and this thread presses what changed.
//!
//! It runs on its own thread, like the guided calibration: the perform thread only copies each frame it polls into the
//! [`FrameTap`], and this thread reads it, runs `omuzaki-track`'s [`GestureDetector`] through the calibration in use,
//! and presses and releases keys through [`PerformHost::press`], the path a hotkey takes. A key nobody bound is
//! skipped. When frames stop (the capture stopped or the source changed) every shown gesture is released.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_track::gesture::{Edge, Gesture, GestureConfig, GestureDetector};
use omuzaki_track_api::body::{BodyBoard, HandGesture, HandGestures};

use super::guided::FrameTap;
use super::host::PerformHost;

/// How often the watch looks for a new frame.
const POLL: Duration = Duration::from_millis(4);

/// With no frame for this long, the shown gestures are released.
const QUIET: Duration = Duration::from_millis(500);

/// How often the calibration in use is read again.
const CALIBRATION_EVERY: Duration = Duration::from_secs(1);

/// The running watch. Dropping it releases what it pressed and stops the thread.
pub struct GestureWatch {
    stop: Arc<AtomicBool>,
    active: Arc<AtomicU8>,
    /// The hand gestures it holds pressed.
    hands: Arc<AtomicU8>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for GestureWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GestureWatch").field("active", &self.active()).finish_non_exhaustive()
    }
}

impl GestureWatch {
    /// Starts watching the frames `tap` gives, and the hand gestures on `board` when there is one, pressing keys on
    /// `perform`.
    ///
    /// # Errors
    /// The thread could not be started.
    pub fn start(tap: Arc<FrameTap>, board: Option<Arc<BodyBoard>>, perform: Arc<dyn PerformHost>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (active, hands) = (Arc::new(AtomicU8::new(0)), Arc::new(AtomicU8::new(0)));
        let (stop_flag, shown, held) = (Arc::clone(&stop), Arc::clone(&active), Arc::clone(&hands));
        let body = move || {
            let mut pressed = HandPresses { board, perform: Arc::clone(&perform), held, last: HandGestures::NONE };
            watch(&tap, perform.as_ref(), &stop_flag, &shown, &mut pressed);
        };
        let thread = std::thread::Builder::new().name("omuzaki-gestures".to_owned()).spawn(body)?;
        Ok(Self { stop, active, hands, thread: Some(thread) })
    }

    /// The hand gestures showing now.
    pub fn hands(&self) -> Vec<HandGesture> {
        let bits = self.hands.load(Ordering::Relaxed);
        HandGesture::ALL.into_iter().enumerate().filter(|(i, _)| bits & (1 << i) != 0).map(|(_, g)| g).collect()
    }

    /// The gestures showing now.
    pub fn active(&self) -> Vec<Gesture> {
        let bits = self.active.load(Ordering::Relaxed);
        Gesture::ALL.into_iter().enumerate().filter(|(i, _)| bits & (1 << i) != 0).map(|(_, g)| g).collect()
    }
}

impl Drop for GestureWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// The hand gestures' side of the watch: presses the keys of the gestures the board shows, releases the ones it
/// stopped showing.
struct HandPresses {
    board: Option<Arc<BodyBoard>>,
    perform: Arc<dyn PerformHost>,
    held: Arc<AtomicU8>,
    last: HandGestures,
}

impl HandPresses {
    /// Presses what changed since the last call, toward `now` (the board's, or none to let go of everything).
    fn sync(&mut self, now: HandGestures) {
        for (i, gesture) in HandGesture::ALL.into_iter().enumerate() {
            let down = now.contains(gesture);
            if down != self.last.contains(gesture) {
                // An unbound key, or a full queue: the gesture shows nothing this time.
                self.perform.press(gesture.key(), down).ok();
                let bit = 1 << i;
                if down {
                    self.held.fetch_or(bit, Ordering::Relaxed);
                } else {
                    self.held.fetch_and(!bit, Ordering::Relaxed);
                }
            }
        }
        self.last = now;
    }

    fn poll(&mut self) {
        let now = self.board.as_ref().map_or(HandGestures::NONE, |board| board.gestures());
        if now != self.last {
            self.sync(now);
        }
    }
}

/// The watch's loop.
fn watch(tap: &FrameTap, perform: &dyn PerformHost, stop: &AtomicBool, active: &AtomicU8, hands: &mut HandPresses) {
    let mut detector = GestureDetector::new(GestureConfig::default());
    let mut calibration = perform.calibration();
    let (mut read_at, mut last_frame) = (Instant::now(), Instant::now());
    let send = |edges: &[Option<Edge>]| {
        for edge in edges.iter().flatten() {
            // An unbound key, or a full queue: the gesture shows nothing this time.
            perform.press(edge.gesture.key(), edge.down).ok();
            let bit = 1 << Gesture::ALL.iter().position(|g| *g == edge.gesture).unwrap_or(0);
            if edge.down {
                active.fetch_or(bit, Ordering::Relaxed);
            } else {
                active.fetch_and(!bit, Ordering::Relaxed);
            }
        }
    };
    while !stop.load(Ordering::Relaxed) {
        hands.poll();
        let Some(frame) = tap.poll_gestures() else {
            if last_frame.elapsed() > QUIET && active.load(Ordering::Relaxed) != 0 {
                send(&detector.reset());
            }
            std::thread::sleep(POLL);
            continue;
        };
        last_frame = Instant::now();
        if read_at.elapsed() > CALIBRATION_EVERY {
            calibration = perform.calibration();
            read_at = Instant::now();
        }
        send(&detector.update(&frame, &calibration));
    }
    send(&detector.reset());
    hands.sync(HandGestures::NONE);
}
