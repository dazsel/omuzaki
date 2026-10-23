//! The tracker section of the live metrics (PF-26, `docs/research/2026-10-live-metrics-snapshot.md` §1, §2, §5).
//!
//! [`Tracker::latency`] is a cold call (it may lock or allocate), so neither the perform loop nor the frame loop ever
//! makes it. Instead a 10 Hz tick on the control thread that owns the tracker copies its [`LatencyStats`] into the
//! [`TrackerWriter`]: a plain copy of a few words, no hand-off to another thread, nothing allocated.
//!
//! - [`TrackerTick`]: for a control loop that already runs: call [`tick`](TrackerTick::tick) as often as it likes, and
//!   it publishes at most every [`TICK`].
//! - [`TrackerTicker`]: a thread of its own, for an owner without a loop: it reads the stats through a closure (which
//!   locks the owner) every [`TICK`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_metrics_api::TrackerWriter;
use omuzaki_metrics_api::sections::TrackerMetrics;
use omuzaki_track_api::tracker::{LatencyStats, Tracker};

/// How often the tracker section is refreshed: 10 Hz.
pub const TICK: Duration = Duration::from_millis(100);

/// The tracker section for `stats`, published at `at`.
pub fn section(at: Seconds, stats: &LatencyStats) -> TrackerMetrics {
    let ms = |d: Duration| d.as_secs_f32() * 1e3;
    let mut m = TrackerMetrics::new(at);
    m.track_ms_p50 = ms(stats.p50);
    m.track_ms_p95 = ms(stats.p95);
    m.track_ms_max = ms(stats.max);
    m.tracker_frames = stats.frames;
    m.tracker_dropped = stats.dropped;
    m
}

/// Publishes the tracker's latency at most every [`TICK`]. Owned by the control thread that owns the tracker.
#[derive(Debug)]
pub struct TrackerTick {
    writer: TrackerWriter,
    /// When the next publish is due; `None` before the first.
    due: Option<f64>,
}

impl TrackerTick {
    /// A tick over the tracker section's only writer.
    pub fn new(writer: TrackerWriter) -> Self {
        Self { writer, due: None }
    }

    /// Publishes `tracker`'s latency if [`TICK`] has passed since the last publish; returns whether it did.
    /// Allocates nothing itself (what [`Tracker::latency`] costs is the tracker's).
    pub fn tick(&mut self, now: Seconds, tracker: &dyn Tracker) -> bool {
        // A nanosecond of slack: a loop at 60 Hz reaches 0.1 s as 6/60, which floats put a hair below 0.1.
        if self.due.is_some_and(|due| now.0 < due - 1e-9) {
            return false;
        }
        self.publish(now, &tracker.latency());
        true
    }

    /// Publishes `stats` now. The next one is due a [`TICK`] after the last due time, so the rate holds at 10 Hz
    /// whatever the caller's rate; a caller that fell more than a tick behind starts again from `now`.
    pub fn publish(&mut self, now: Seconds, stats: &LatencyStats) {
        self.writer.publish(&section(now, stats));
        let tick = TICK.as_secs_f64();
        self.due = Some(match self.due {
            Some(due) if now.0 < due + tick => due + tick,
            _ => now.0 + tick,
        });
    }
}

/// The tick on a thread of its own. Dropping it stops the thread.
pub struct TrackerTicker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<TrackerTick>>,
}

impl TrackerTicker {
    /// Every [`TICK`] of `clock`, publishes what `read` returns (the owner's `Tracker::latency`, or `None` while no
    /// capture runs, which publishes nothing). Cold.
    ///
    /// # Errors
    /// The OS refused a thread.
    pub fn start(
        writer: TrackerWriter,
        clock: Arc<dyn Clock>,
        mut read: impl FnMut() -> Option<LatencyStats> + Send + 'static,
    ) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new().name("omuzaki-tracker-metrics".into()).spawn(move || {
            let mut tick = TrackerTick::new(writer);
            while !flag.load(Ordering::Relaxed) {
                if let Some(stats) = read() {
                    tick.publish(clock.now(), &stats);
                }
                std::thread::park_timeout(TICK);
            }
            tick
        })?;
        Ok(Self { stop, thread: Some(thread) })
    }

    /// Stops the thread and returns the tick (and with it the writer).
    pub fn stop(mut self) -> Option<TrackerTick> {
        self.halt()
    }

    fn halt(&mut self) -> Option<TrackerTick> {
        self.stop.store(true, Ordering::Relaxed);
        let thread = self.thread.take()?;
        thread.thread().unpark();
        thread.join().ok()
    }
}

impl Drop for TrackerTicker {
    fn drop(&mut self) {
        let _ = self.halt();
    }
}
