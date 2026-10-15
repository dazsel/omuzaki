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
use super::preview::{PreviewFrame, PreviewTap};
use super::{Failures, FrameTracker};
use crate::sample::{FaceSample, to_track_frame};

/// The longest the thread waits inside the camera, so `stop` is noticed within this (the join budget is 200 ms).
const POLL: Duration = Duration::from_millis(20);

/// How long `stop` waits for the camera thread to come back from a frame in hand before it detaches it.
pub(super) const JOIN_GRACE: Duration = Duration::from_millis(150);

// `stop` waits at most the grace period, which covers a camera poll and the frame in hand; the task's budget for it
// is 200 ms.
const _: () = assert!(POLL.as_millis() + JOIN_GRACE.as_millis() <= 200);

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

/// Everything the camera thread owns or shares.
pub(super) struct Run {
    pub(super) stream: Box<dyn CameraStream>,
    pub(super) tracker: Box<dyn FrameTracker>,
    pub(super) shared: Arc<Shared>,
    /// Track one camera frame in this many (see `tier_stride`).
    pub(super) stride: u32,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) preview: Arc<PreviewTap>,
    pub(super) error: Arc<Mutex<Option<TrackError>>>,
}

/// What the thread hands back when it ends: the tracker, for the next session.
pub(super) struct Outcome {
    pub(super) tracker: Box<dyn FrameTracker>,
}

/// Numbers frames and keeps their stage times strictly increasing, whichever path stamps them.
struct Publisher {
    writer: MailboxWriter,
    /// The `seq` of the next frame.
    seq: u64,
    /// The `seq` of the last frame published, which a notice repeats.
    last_seq: Option<u64>,
    last_time: f64,
}

/// When the last frame went out, for the watchdog to read without taking the publisher's lock.
struct Pulse {
    /// Stage time of the last publish, as `f64` bits.
    last: AtomicU64,
    /// A notice has been sent for the silence since `last`; the next frame clears it.
    notified: AtomicBool,
}

/// What the camera thread and the watchdog share: the one writer of the mailbox, and the clock and counters.
///
/// The camera thread takes the lock once per frame, to publish; the watchdog takes it only when a silence has gone on
/// for `lost_after`, and checks again under it, so a frame that went out meanwhile is never overwritten by a stale
/// notice. A frame published by a thread that was stuck and came back is ordered after the notice, never before it.
pub(super) struct Shared {
    publisher: Mutex<Publisher>,
    pulse: Pulse,
    clock: Arc<dyn Clock>,
    pub(super) stats: Arc<Stats>,
}

impl Shared {
    pub(super) fn new(writer: MailboxWriter, first_seq: u64, clock: Arc<dyn Clock>, stats: Arc<Stats>) -> Self {
        let pulse = Pulse { last: AtomicU64::new(clock.now().0.to_bits()), notified: AtomicBool::new(false) };
        let publisher = Mutex::new(Publisher { writer, seq: first_seq, last_seq: None, last_time: f64::NEG_INFINITY });
        Self { publisher, pulse, clock, stats }
    }

    /// The `seq` the next frame gets.
    pub(super) fn next_seq(&self) -> u64 {
        self.publisher.lock().unwrap_or_else(PoisonError::into_inner).seq
    }

    /// Publishes a frame, a lost one if `sample` is `None`, no earlier than `time`. Returns it with the stage time
    /// at which it went out.
    fn publish(&self, sample: Option<&FaceSample>, time: Seconds) -> (TrackFrame, Seconds) {
        let mut out = self.publisher.lock().unwrap_or_else(PoisonError::into_inner);
        let time = Seconds(time.0.max(out.last_time + MIN_STEP));
        out.last_time = time.0;
        let frame = to_track_frame(sample, out.seq, time);
        out.writer.publish(&frame);
        out.last_seq = Some(out.seq);
        out.seq += 1;
        let now = self.clock.now();
        self.pulse.last.store(now.0.to_bits(), Ordering::Relaxed);
        self.pulse.notified.store(false, Ordering::Relaxed);
        self.stats.published.fetch_add(1, Ordering::Relaxed);
        (frame, now)
    }

    /// Publishes one lost frame that stands for no camera frame, so it repeats the last frame's `seq` and takes no
    /// new one (`track-api.md` §8). With `silence` it is the watchdog's: sent only if no frame has gone out for that
    /// long and none has been sent for this silence, and not at all before the first frame, when there is no face to
    /// clear. Without it the camera is gone, and the notice is sent whatever came before.
    fn notify(&self, silence: Option<Duration>) -> Option<TrackFrame> {
        let mut out = self.publisher.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.clock.now();
        if let Some(silence) = silence {
            let quiet = now.0 - f64::from_bits(self.pulse.last.load(Ordering::Relaxed)) >= silence.as_secs_f64();
            if !quiet || self.pulse.notified.swap(true, Ordering::Relaxed) {
                return None;
            }
        } else {
            self.pulse.notified.store(true, Ordering::Relaxed);
        }
        let seq = out.last_seq.or_else(|| silence.is_none().then_some(out.seq))?;
        let time = Seconds(now.0.max(out.last_time + MIN_STEP));
        out.last_time = time.0;
        let frame = to_track_frame(None, seq, time);
        out.writer.publish(&frame);
        self.stats.published.fetch_add(1, Ordering::Relaxed);
        Some(frame)
    }
}

/// The watchdog: publishes the one lost frame when nothing has gone out for `lost_after`, whether the camera is
/// silent or the tracker is stuck inside `track`, which the camera thread cannot notice from the inside. Ends at `stop`.
pub(super) fn watch(shared: &Shared, lost_after: Duration, stop: &AtomicBool) {
    let tick = lost_after.min(POLL).max(Duration::from_millis(1));
    while !stop.load(Ordering::Relaxed) {
        shared.notify(Some(lost_after));
        // Unparked by `stop`; a spurious wake-up only checks again.
        std::thread::park_timeout(tick);
    }
}

impl Run {
    /// Runs until `stop` is set or the stream fails. A failure publishes one lost frame and is kept in `error`.
    pub(super) fn run(self) -> Outcome {
        let Self { mut stream, mut tracker, shared, stride, stop, preview, error } = self;
        let stats = Arc::clone(&shared.stats);
        // Sized for the mode the camera opened with, so the first frame allocates nothing either.
        let mode = stream.format();
        let mut converter = FrameConverter::with_size(mode.width, mode.height);
        let (mut device_clock, mut scratch) = (ClockMap::new(), PreviewFrame::default());
        let mut camera_seq = None::<u64>;
        let mut accepted = 0_u64;
        while !stop.load(Ordering::Relaxed) {
            let frame = match stream.next_frame(POLL) {
                Ok(Some(frame)) => frame,
                // A silent camera is the watchdog's to report.
                Ok(None) => continue,
                Err(e) => {
                    // Kept before the lost frame goes out, so a consumer that sees it can already ask why.
                    *error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
                    shared.notify(None);
                    break;
                }
            };
            let arrived = shared.clock.now();
            if let Some(previous) = camera_seq {
                stats.dropped.fetch_add(frame.seq.saturating_sub(previous + 1), Ordering::Relaxed);
            }
            camera_seq = Some(frame.seq);
            // A device time before the last accepted one is a glitch, not a new epoch: the contract is to drop the
            // frame and keep the fit.
            let Ok(time) = device_clock.map(frame.captured, arrived) else {
                stats.backwards.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            // A lowered tier skips frames between the ones it tracks.
            accepted += 1;
            if !(accepted - 1).is_multiple_of(u64::from(stride.max(1))) {
                continue;
            }
            // A frame that cannot be converted or tracked is a lost frame: the pipeline must not hold a stale face.
            let Ok(rgb) = converter.convert(&frame) else {
                stats.conversion.fetch_add(1, Ordering::Relaxed);
                shared.publish(None, time);
                continue;
            };
            let sample = if let Ok((sample, _timings)) = tracker.track(&rgb, time) {
                sample
            } else {
                stats.tracking.fetch_add(1, Ordering::Relaxed);
                None
            };
            let (published, now) = shared.publish(sample.as_ref(), time);
            // Before the preview, so that work cannot inflate the health metric.
            stats.record(Seconds(now.0 - published.time.0));
            preview.offer(&mut scratch, &rgb, published.seq, sample.as_ref().map(|s| &s.landmarks));
        }
        Outcome { tracker }
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
