//! The camera thread: camera frame, pixel conversion, face tracker, stage-clock time, mailbox, preview.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use omuzaki_anim_api::TrackFrame;
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_track_api::camera::CameraStream;
use omuzaki_track_api::clock::{ClockMap, MIN_STEP};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::mailbox::MailboxWriter;
use omuzaki_track_api::tracker::LatencyStats;

use super::convert::FrameConverter;
use super::preview::{PreviewTap, Scratch};
use super::{Failures, FrameTracker};
use crate::sample::{FaceSample, to_track_frame};

/// The longest the thread waits inside the camera, so `stop` is noticed within this (the join budget is 200 ms).
const POLL: Duration = Duration::from_millis(20);

// `stop` waits at most one poll for the camera, plus the frame in hand; the task's budget for it is 200 ms.
const _: () = assert!(POLL.as_millis() * 5 <= 200);

/// Width of one latency bucket, in microseconds.
const BUCKET_US: u64 = 100;

/// Latency buckets: 0 to about 100 ms; anything slower lands in the last one (the exact worst case is kept apart).
const BUCKETS: usize = 1024;

/// Counters the camera thread writes with atomic adds and [`LatencyStats`] reads: no lock, and neither side
/// allocates or sorts. Latency goes into a fixed histogram of the whole session, so a percentile is a scan of
/// [`BUCKETS`] counters, accurate to [`BUCKET_US`].
pub(super) struct Stats {
    published: AtomicU64,
    /// Camera frames skipped, by gaps in `CameraFrame::seq`. Nothing else is counted here.
    dropped: AtomicU64,
    backwards: AtomicU64,
    conversion: AtomicU64,
    tracking: AtomicU64,
    samples: AtomicU64,
    max_us: AtomicU32,
    histogram: [AtomicU64; BUCKETS],
}

impl Stats {
    pub(super) fn new() -> Self {
        Self {
            published: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            backwards: AtomicU64::new(0),
            conversion: AtomicU64::new(0),
            tracking: AtomicU64::new(0),
            samples: AtomicU64::new(0),
            max_us: AtomicU32::new(0),
            histogram: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn record(&self, latency: Seconds) {
        // Saturating float to int: a latency is never negative, and over an hour is not a latency.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let us = (latency.0.max(0.0) * 1e6) as u32;
        let bucket = (u64::from(us) / BUCKET_US).min(BUCKETS as u64 - 1);
        self.histogram[bucket as usize].fetch_add(1, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
        self.samples.fetch_add(1, Ordering::Release);
    }

    /// The upper edge of the bucket holding the `q`th percentile, never above the worst case seen.
    fn percentile(&self, samples: u64, q: u64) -> u64 {
        let rank = (samples * q).div_ceil(100).max(1);
        let mut seen = 0;
        for (i, count) in self.histogram.iter().enumerate() {
            seen += count.load(Ordering::Relaxed);
            if seen >= rank {
                return (i as u64 + 1) * BUCKET_US;
            }
        }
        u64::from(self.max_us.load(Ordering::Relaxed))
    }

    /// Cheap: a scan of the histogram, no allocation, no sort.
    pub(super) fn snapshot(&self) -> LatencyStats {
        let samples = self.samples.load(Ordering::Acquire);
        let max = u64::from(self.max_us.load(Ordering::Relaxed));
        let at = |q| Duration::from_micros(if samples == 0 { 0 } else { self.percentile(samples, q).min(max) });
        LatencyStats::new(
            self.published.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            at(50),
            at(95),
            Duration::from_micros(max),
        )
    }

    pub(super) fn failures(&self) -> Failures {
        Failures::new(
            self.backwards.load(Ordering::Relaxed),
            self.conversion.load(Ordering::Relaxed),
            self.tracking.load(Ordering::Relaxed),
        )
    }
}

/// Everything one session's thread owns or shares.
pub(super) struct Run {
    pub(super) stream: Box<dyn CameraStream>,
    pub(super) tracker: Box<dyn FrameTracker>,
    pub(super) writer: MailboxWriter,
    pub(super) clock: Arc<dyn Clock>,
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
        let Self { mut stream, mut tracker, writer, clock, first_seq, lost_after, stop, preview, stats, error } = self;
        let mut out = Publisher { writer, seq: first_seq, last: f64::NEG_INFINITY, stats: Arc::clone(&stats) };
        // Sized for the mode the camera opened with, so the first frame allocates nothing either.
        let mode = stream.format();
        let mut converter = FrameConverter::with_size(mode.width, mode.height);
        let (mut device_clock, mut scratch) = (ClockMap::new(), Scratch::default());
        let timeout = lost_after.min(POLL);
        let mut last_arrival = clock.now().0;
        let mut told_lost = false;
        let mut camera_seq = None::<u64>;
        while !stop.load(Ordering::Relaxed) {
            let frame = match stream.next_frame(timeout) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    let now = clock.now();
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
                    out.publish(None, clock.now());
                    break;
                }
            };
            let arrived = clock.now();
            if let Some(previous) = camera_seq {
                stats.dropped.fetch_add(frame.seq.saturating_sub(previous + 1), Ordering::Relaxed);
            }
            camera_seq = Some(frame.seq);
            // A device time before the last accepted one is a glitch, not a new epoch: the contract is to drop the
            // frame and keep the fit. It does not count as the camera being alive, so `lost_after` still runs.
            let Ok(time) = device_clock.map(frame.captured, arrived) else {
                stats.backwards.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            (last_arrival, told_lost) = (arrived.0, false);
            // A frame that cannot be converted or tracked is a lost frame: the pipeline must not hold a stale face.
            let Ok(rgb) = converter.convert(&frame) else {
                stats.conversion.fetch_add(1, Ordering::Relaxed);
                out.publish(None, time);
                continue;
            };
            let sample = if let Ok((sample, _timings)) = tracker.track(&rgb, time) {
                sample
            } else {
                stats.tracking.fetch_add(1, Ordering::Relaxed);
                None
            };
            let published = out.publish(sample.as_ref(), time);
            // Before the preview, so that work cannot inflate the health metric.
            stats.record(Seconds(clock.now().0 - published.time.0));
            preview.offer(&mut scratch, &rgb, published.seq, sample.as_ref().map(|s| &s.landmarks));
        }
        Outcome { tracker, next_seq: out.seq }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_come_from_the_histogram_and_never_exceed_the_worst_case() {
        let stats = Stats::new();
        assert_eq!(stats.snapshot(), LatencyStats::default());
        for _ in 0..95 {
            stats.record(Seconds(0.001));
        }
        for _ in 0..5 {
            stats.record(Seconds(0.010));
        }
        let latency = stats.snapshot();
        let within = |got: Duration, want_us: u64| {
            let got = u64::try_from(got.as_micros()).unwrap();
            assert!((want_us..=want_us + BUCKET_US).contains(&got), "{got} us, wanted {want_us} us");
        };
        within(latency.p50, 1000);
        within(latency.p95, 1000);
        assert_eq!(latency.max, Duration::from_millis(10));
        // A latency past the last bucket is clamped there, and the worst case stays exact.
        stats.record(Seconds(5.0));
        assert_eq!(stats.snapshot().max, Duration::from_secs(5));
        assert!(stats.snapshot().p95 <= Duration::from_micros(BUCKETS as u64 * BUCKET_US));
    }

    #[test]
    fn failures_and_drops_are_counted_apart() {
        let stats = Stats::new();
        stats.backwards.fetch_add(2, Ordering::Relaxed);
        stats.conversion.fetch_add(3, Ordering::Relaxed);
        stats.tracking.fetch_add(4, Ordering::Relaxed);
        assert_eq!(stats.failures(), Failures::new(2, 3, 4));
        assert_eq!(stats.snapshot().dropped, 0);
    }
}
