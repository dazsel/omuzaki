//! The camera thread: camera frame, pixel conversion, face tracker, stage-clock time, mailbox, preview.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_pixfmt::MjpgDecoder;
use omuzaki_track_api::camera::CameraStream;
use omuzaki_track_api::clock::{ClockMap, MIN_STEP};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::mailbox::MailboxWriter;
use omuzaki_track_api::tracker::LatencyStats;

use super::convert::{convert_into, rgb_len};
use super::preview::{PreviewFrame, PreviewTap};
use super::{Failures, FrameTracker};
use crate::error::FaceTrackError;
use crate::frame::RgbFrame;
use crate::sample::{FaceSample, StageTimings, to_track_frame};

/// The longest the thread waits inside the camera, so `stop` is noticed within this (the join budget is 200 ms).
pub(super) const POLL: Duration = Duration::from_millis(20);

/// How long the camera thread waits at its end for the tracker's thread, which `stop` then waits for in turn (and `stop`
/// has two polls on top); a tracker that is not back by then is let go.
pub(super) const JOIN_GRACE: Duration = Duration::from_millis(100);

// `stop` waits at most the grace period and two camera polls (the camera thread's and its own); the task's budget
// for it is 200 ms.
const _: () = assert!(JOIN_GRACE.as_millis() + 2 * POLL.as_millis() <= 200);

/// How long a thread blocks on an empty channel to warm up its blocking path (see the tracker thread's loop).
const WARM_UP: Duration = Duration::from_micros(1);

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

    /// The upper edge of the bucket holding the `q`th percentile (the worst case, for the last), never above the worst
    /// case seen.
    fn percentile(&self, samples: u64, q: u64) -> u64 {
        let rank = (samples * q).div_ceil(100).max(1);
        let mut seen = 0;
        for (i, count) in self.histogram.iter().enumerate() {
            seen += count.load(Ordering::Relaxed);
            if seen >= rank {
                // The last bucket holds everything slower than the range, so only the worst case speaks for it.
                return if i == BUCKETS - 1 { u64::MAX } else { (i as u64 + 1) * BUCKET_US };
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

/// A frame for the worker to track: the converted pixels, and the capture time on the stage clock.
struct Job {
    rgb: Vec<u8>,
    width: u32,
    height: u32,
    time: Seconds,
}

/// What the worker answers: the pixels back, for the next frame, and what the tracker made of them.
struct Done {
    rgb: Vec<u8>,
    result: Result<(Option<FaceSample>, StageTimings), FaceTrackError>,
}

/// Everything the camera thread owns.
pub(super) struct Run {
    pub(super) stream: Box<dyn CameraStream>,
    pub(super) writer: MailboxWriter,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) first_seq: u64,
    pub(super) lost_after: Duration,
    /// Track one camera frame in this many (see `tier_stride`).
    pub(super) stride: u32,
    /// How long to wait for the tracker's thread at the end before letting it go.
    pub(super) grace: Duration,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) preview: Arc<PreviewTap>,
    pub(super) stats: Arc<Stats>,
    pub(super) error: Arc<Mutex<Option<TrackError>>>,
    pub(super) worker: Worker,
}

/// What the thread hands back when it ends: the tracker, if its thread came back, and the next free `seq`.
pub(super) struct Outcome {
    pub(super) tracker: Option<Box<dyn FrameTracker>>,
    pub(super) next_seq: u64,
}

/// What the worker thread runs, until its jobs end; it returns the tracker.
pub(super) type Body = Box<dyn FnOnce() -> Option<Box<dyn FrameTracker>> + Send>;

/// The inference thread and the two bounded channels to it, made by `start` before the camera thread.
pub(super) struct Worker {
    handle: JoinHandle<Option<Box<dyn FrameTracker>>>,
    jobs: SyncSender<Job>,
    done: Receiver<Done>,
}

impl Worker {
    /// Starts the thread that owns `tracker` and answers each [`Job`] over a one-slot channel, so neither side
    /// allocates per frame. `spawn` is how `start` makes threads, so that it can fail one on purpose.
    pub(super) fn start(
        tracker: Box<dyn FrameTracker>,
        spawn: impl FnOnce(Body) -> io::Result<JoinHandle<Option<Box<dyn FrameTracker>>>>,
    ) -> Result<Self, (io::Error, Box<dyn FrameTracker>)> {
        let (jobs, job_rx) = sync_channel::<Job>(1);
        let (done_tx, done) = sync_channel::<Done>(1);
        // The tracker travels in a cell, so a closure dropped by a failed spawn does not take it along.
        let cell = Arc::new(Mutex::new(Some(tracker)));
        let body = {
            let cell = Arc::clone(&cell);
            Box::new(move || {
                let mut tracker = cell.lock().unwrap_or_else(PoisonError::into_inner).take()?;
                // The first time a thread blocks on a channel, std allocates (a per-thread wait context and the
                // channel's list of waiters). Block once now, for a microsecond, so that happens before the first
                // frame whatever the timing, and the steady state allocates nothing.
                let mut pending = match job_rx.recv_timeout(WARM_UP) {
                    Ok(job) => Some(job),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return Some(tracker),
                };
                while let Some(Job { rgb, width, height, time }) = pending.take().or_else(|| job_rx.recv().ok()) {
                    let frame = RgbFrame { data: &rgb, width, height, stride: 3 * width as usize };
                    let result = tracker.track(&frame, time);
                    if done_tx.send(Done { rgb, result }).is_err() {
                        break;
                    }
                }
                Some(tracker)
            }) as Body
        };
        match spawn(body) {
            Ok(handle) => Ok(Self { handle, jobs, done }),
            Err(e) => match cell.lock().unwrap_or_else(PoisonError::into_inner).take() {
                Some(tracker) => Err((e, tracker)),
                None => unreachable!("a thread that did not start has not taken the tracker"),
            },
        }
    }

    /// Ends the thread and takes the tracker back, if it comes within `grace`; otherwise lets the thread go.
    pub(super) fn finish(self, grace: Duration) -> Option<Box<dyn FrameTracker>> {
        let Self { handle, jobs, done } = self;
        // With the sending end gone the thread leaves its loop as soon as it is idle.
        drop((jobs, done));
        let give_up = Instant::now() + grace;
        while !handle.is_finished() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(1));
        }
        if handle.is_finished() { handle.join().ok().flatten() } else { None }
    }
}

/// Numbers frames and keeps their stage times strictly increasing, whichever path stamps them. Owned by the camera
/// thread alone: nothing else ever publishes, so publication takes no lock.
struct Publisher {
    writer: MailboxWriter,
    clock: Arc<dyn Clock>,
    stats: Arc<Stats>,
    lost_after: f64,
    /// The `seq` of the next frame.
    seq: u64,
    /// The `seq` of the last frame published, which a notice repeats.
    last_seq: Option<u64>,
    /// The stage time of the last frame published, which the next must follow.
    last_time: f64,
    /// When anything last went out, for the silence rule.
    last_publish: f64,
    /// A notice has been sent for the silence since `last_publish`.
    notified: bool,
}

impl Publisher {
    /// Publishes a frame, a lost one if `sample` is `None`, stamped no earlier than `time` and after the last one.
    /// Returns its `seq` and the stage time at which it went out.
    fn publish(&mut self, sample: Option<&FaceSample>, time: Seconds) -> (u64, Seconds) {
        let time = Seconds(time.0.max(self.last_time + MIN_STEP));
        self.last_time = time.0;
        let seq = self.seq;
        self.writer.publish(&to_track_frame(sample, seq, time));
        (self.last_seq, self.seq) = (Some(seq), seq + 1);
        let now = self.clock.now();
        (self.last_publish, self.notified) = (now.0, false);
        self.stats.published.fetch_add(1, Ordering::Relaxed);
        (seq, now)
    }

    /// Publishes the one lost frame that stands for no camera frame, so it repeats the last frame's `seq` and takes
    /// no new one (`track-api.md` §8). `silent` is the stall rule's: sent only once nothing has gone out for
    /// `lost_after`, once per silence, and not at all before the first frame, when there is no face to clear. The
    /// camera being gone is not silent: it is sent whatever came before.
    fn notify(&mut self, silent: bool) {
        let now = self.clock.now();
        if silent {
            if self.notified || now.0 - self.last_publish < self.lost_after {
                return;
            }
            self.notified = true;
        }
        let Some(seq) = self.last_seq.or_else(|| (!silent).then_some(self.seq)) else { return };
        let time = Seconds(now.0.max(self.last_time + MIN_STEP));
        self.last_time = time.0;
        self.writer.publish(&to_track_frame(None, seq, time));
        self.stats.published.fetch_add(1, Ordering::Relaxed);
    }
}

/// Why the wait for the worker's answer ended without one.
enum Ended {
    /// `stop` was asked for while waiting.
    Stopped,
    /// The worker's thread is gone: it panicked.
    Died,
}

impl Run {
    /// Runs until `stop` is set or the stream fails. A failure publishes one lost frame and is kept in `error`.
    pub(super) fn run(self) -> Outcome {
        let Self {
            mut stream,
            writer,
            clock,
            first_seq,
            lost_after,
            stride,
            grace,
            stop,
            preview,
            stats,
            error,
            worker,
        } = self;
        let mut out = Publisher {
            writer,
            lost_after: lost_after.as_secs_f64(),
            last_publish: clock.now().0,
            clock: Arc::clone(&clock),
            stats: Arc::clone(&stats),
            seq: first_seq,
            last_seq: None,
            last_time: f64::NEG_INFINITY,
            notified: false,
        };
        // Sized for the mode the camera opened with, so the first frame allocates nothing either.
        let mode = stream.format();
        let mut mjpg = MjpgDecoder::new();
        let mut rgb = vec![0; rgb_len(mode.width, mode.height)];
        let (mut device_clock, mut scratch) = (ClockMap::new(), PreviewFrame::default());
        let mut camera_seq = None::<u64>;
        let mut accepted = 0_u64;
        // The same warm-up as the tracker's thread: nothing is waiting yet, so this times out at once.
        worker.done.recv_timeout(WARM_UP).ok();
        let fail = |why: TrackError| *error.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
        'frames: while !stop.load(Ordering::Relaxed) {
            let frame = match stream.next_frame(POLL) {
                Ok(Some(frame)) => frame,
                // A silent camera: say so once `lost_after` has gone by.
                Ok(None) => {
                    out.notify(true);
                    continue;
                }
                Err(e) => {
                    // Kept before the lost frame goes out, so a consumer that sees it can already ask why.
                    fail(e);
                    out.notify(false);
                    break;
                }
            };
            let arrived = clock.now();
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
            if convert_into(&mut mjpg, &frame, &mut rgb).is_err() {
                stats.conversion.fetch_add(1, Ordering::Relaxed);
                out.publish(None, time);
                continue;
            }
            let job = Job { rgb: std::mem::take(&mut rgb), width: frame.width, height: frame.height, time };
            if worker.jobs.try_send(job).is_err() {
                fail(TrackError::Backend("the face tracker's thread is gone".to_owned()));
                out.notify(false);
                break;
            }
            // The camera thread does not run inference, so it can keep time while the tracker works: it waits a tick
            // at a time, says so if the silence passes `lost_after`, and looks at `stop`, so neither a slow tracker
            // nor one that never returns can hold up the lost frame or the end.
            let answer = loop {
                match worker.done.recv_timeout(POLL) {
                    Ok(done) => break Ok(done),
                    Err(RecvTimeoutError::Timeout) if stop.load(Ordering::Relaxed) => break Err(Ended::Stopped),
                    Err(RecvTimeoutError::Timeout) => out.notify(true),
                    Err(RecvTimeoutError::Disconnected) => break Err(Ended::Died),
                }
            };
            let Ok(Done { rgb: pixels, result }) = answer else {
                if matches!(answer, Err(Ended::Died)) {
                    fail(TrackError::Backend("the face tracker's thread panicked".to_owned()));
                    out.notify(false);
                }
                break 'frames;
            };
            rgb = pixels;
            let sample = if let Ok((sample, _timings)) = result {
                sample
            } else {
                stats.tracking.fetch_add(1, Ordering::Relaxed);
                None
            };
            let (seq, now) = out.publish(sample.as_ref(), time);
            // Latency is from when the frame was captured, not from the time its output was ordered at, and it is
            // taken before the preview, so that work cannot inflate it.
            stats.record(Seconds(now.0 - time.0));
            let frame =
                RgbFrame { data: &rgb, width: frame.width, height: frame.height, stride: 3 * frame.width as usize };
            preview.offer(&mut scratch, &frame, seq, sample.as_ref().map(|s| &s.landmarks));
        }
        let tracker = worker.finish(grace);
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
        // 5 s is past the last bucket: 1 of 101 samples, so the 10 ms group still holds p95.
        within(stats.snapshot().p95, 10_000);
        for _ in 0..200 {
            stats.record(Seconds(0.250));
        }
        assert_eq!(stats.snapshot().p50, Duration::from_secs(5), "the overflow bucket speaks with the worst case");
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
