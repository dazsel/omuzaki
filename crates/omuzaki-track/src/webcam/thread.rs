//! The camera thread: camera frame, pixel conversion, face tracker, stage-clock time, mailbox, preview.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use omuzaki_anim_api::TrackFrame;
use omuzaki_core::time::Seconds;
use omuzaki_track_api::camera::CameraStream;
use omuzaki_track_api::clock::{ClockMap, MIN_STEP};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::mailbox::MailboxWriter;
use omuzaki_track_api::tracker::LatencyStats;

use super::FrameTracker;
use super::convert::FrameConverter;
use super::preview::{PreviewTap, Scratch};
use crate::sample::{FaceSample, to_track_frame};

/// The longest the thread waits inside the camera, so `stop` is noticed within this (the join budget is 200 ms).
const POLL: Duration = Duration::from_millis(20);

/// Latency samples kept, newest overwriting oldest.
const WINDOW: usize = 512;

/// Seconds on the stage clock: monotonic, from the first call in the process. The webcam's own source of "now" for
/// [`ClockMap::map`], as the contract leaves the clock to the tracker.
pub(super) fn stage_now() -> Seconds {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    Seconds(ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64())
}

/// Counters the thread writes with plain atomic stores and [`LatencyStats`] reads: no lock on the hot path.
pub(super) struct Stats {
    published: AtomicU64,
    dropped: AtomicU64,
    samples: AtomicUsize,
    max_us: AtomicU32,
    ring: [AtomicU32; WINDOW],
}

impl Stats {
    pub(super) fn new() -> Self {
        Self {
            published: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            samples: AtomicUsize::new(0),
            max_us: AtomicU32::new(0),
            ring: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    fn record(&self, latency: Seconds) {
        // Saturating float to int: a latency is never negative or over an hour in practice.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let us = (latency.0.max(0.0) * 1e6) as u32;
        let n = self.samples.load(Ordering::Relaxed);
        self.ring[n % WINDOW].store(us, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
        self.samples.store(n.wrapping_add(1), Ordering::Release);
    }

    /// Cold: sorts a copy of the window.
    pub(super) fn snapshot(&self) -> LatencyStats {
        let n = self.samples.load(Ordering::Acquire);
        let mut window: Vec<u32> = self.ring[..n.min(WINDOW)].iter().map(|c| c.load(Ordering::Relaxed)).collect();
        window.sort_unstable();
        let at =
            |q: usize| window.get((window.len() * q / 100).min(window.len().saturating_sub(1))).copied().unwrap_or(0);
        let us = |v: u32| Duration::from_micros(u64::from(v));
        LatencyStats::new(
            self.published.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            us(at(50)),
            us(at(95)),
            us(self.max_us.load(Ordering::Relaxed)),
        )
    }
}

/// Everything one session's thread owns or shares.
pub(super) struct Run {
    pub(super) stream: Box<dyn CameraStream>,
    pub(super) tracker: Box<dyn FrameTracker>,
    pub(super) writer: MailboxWriter,
    pub(super) first_seq: u64,
    pub(super) lost_after: Duration,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) preview: Arc<PreviewTap>,
    pub(super) stats: Arc<Stats>,
    pub(super) error: Arc<Mutex<Option<TrackError>>>,
}

/// What the thread hands back when it ends: the tracker, for the next session, and the next free `seq`.
pub(super) struct Outcome {
    pub(super) tracker: Box<dyn FrameTracker>,
    pub(super) next_seq: u64,
}

/// Numbers frames and keeps their stage times strictly increasing, whichever path stamps them.
struct Publisher {
    writer: MailboxWriter,
    seq: u64,
    last: f64,
    stats: Arc<Stats>,
}

impl Publisher {
    /// Publishes `sample` (or a lost frame) no earlier than `time`; returns the frame it published.
    fn publish(&mut self, sample: Option<&FaceSample>, time: Seconds) -> TrackFrame {
        let time = Seconds(time.0.max(self.last + MIN_STEP));
        self.last = time.0;
        let frame = to_track_frame(sample, self.seq, time);
        self.writer.publish(&frame);
        self.seq += 1;
        self.stats.published.fetch_add(1, Ordering::Relaxed);
        frame
    }
}

impl Run {
    /// Runs until `stop` is set or the stream fails. A failure publishes one lost frame and is kept in `error`.
    pub(super) fn run(self) -> Outcome {
        let Self { mut stream, mut tracker, writer, first_seq, lost_after, stop, preview, stats, error } = self;
        let mut out = Publisher { writer, seq: first_seq, last: f64::NEG_INFINITY, stats: Arc::clone(&stats) };
        let (mut converter, mut clock, mut scratch) = (FrameConverter::new(), ClockMap::new(), Scratch::default());
        let timeout = lost_after.min(POLL);
        let mut last_arrival = stage_now().0;
        let mut told_lost = false;
        let mut camera_seq = None::<u64>;
        while !stop.load(Ordering::Relaxed) {
            let frame = match stream.next_frame(timeout) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    let now = stage_now();
                    // One lost frame per silence, so the pipeline never holds a stale face.
                    if !told_lost && now.0 - last_arrival >= lost_after.as_secs_f64() {
                        out.publish(None, now);
                        told_lost = true;
                    }
                    continue;
                }
                Err(e) => {
                    // Kept before the lost frame goes out, so a consumer that sees it can already ask why.
                    *error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
                    out.publish(None, stage_now());
                    break;
                }
            };
            let arrived = stage_now();
            (last_arrival, told_lost) = (arrived.0, false);
            if let Some(previous) = camera_seq {
                stats.dropped.fetch_add(frame.seq.saturating_sub(previous + 1), Ordering::Relaxed);
            }
            camera_seq = Some(frame.seq);
            // A device clock that stepped back starts a new fit; the output still increases.
            let time = clock
                .map(frame.captured, arrived)
                .or_else(|_| {
                    clock.reset();
                    clock.map(frame.captured, arrived)
                })
                .unwrap_or(arrived);
            // A frame that cannot be converted or tracked is a lost frame: the pipeline must not hold a stale face.
            let Ok(rgb) = converter.convert(&frame) else {
                out.publish(None, time);
                stats.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let sample = if let Ok((sample, _timings)) = tracker.track(&rgb, time) {
                sample
            } else {
                stats.dropped.fetch_add(1, Ordering::Relaxed);
                None
            };
            let published = out.publish(sample.as_ref(), time);
            preview.offer(&mut scratch, &rgb, published.seq, sample.as_ref().map(|s| &s.landmarks));
            stats.record(Seconds(stage_now().0 - published.time.0));
        }
        Outcome { tracker, next_seq: out.seq }
    }
}
