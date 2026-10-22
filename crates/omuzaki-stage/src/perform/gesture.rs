//! The gesture watch (spec v3 PF-25): face gestures from the running capture press their trigger keys, so a
//! performer who binds `gesture:tongue_out` to "bleh" with `trigger.bind` shows it by sticking their tongue out.
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
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for GestureWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GestureWatch").field("active", &self.active()).finish_non_exhaustive()
    }
}

impl GestureWatch {
    /// Starts watching the frames `tap` gives, pressing keys on `perform`.
    ///
    /// # Errors
    /// The thread could not be started.
    pub fn start(tap: Arc<FrameTap>, perform: Arc<dyn PerformHost>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicU8::new(0));
        let (stop_flag, shown) = (Arc::clone(&stop), Arc::clone(&active));
        let body = move || watch(&tap, perform.as_ref(), &stop_flag, &shown);
        let thread = std::thread::Builder::new().name("omuzaki-gestures".to_owned()).spawn(body)?;
        Ok(Self { stop, active, thread: Some(thread) })
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

/// The watch's loop.
fn watch(tap: &FrameTap, perform: &dyn PerformHost, stop: &AtomicBool, active: &AtomicU8) {
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
}
