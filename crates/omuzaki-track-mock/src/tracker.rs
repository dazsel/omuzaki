//! `FakeTracker`: scripted [`TrackFrame`]s released at their scripted stage times.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use omuzaki_anim_api::{FrameSource, TrackFrame, TrackStatus};
use omuzaki_core::time::Seconds;
use omuzaki_track_api::camera::{CameraFormat, CameraId, CameraInfo, PixelFormat};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};

use crate::FakeCamera;

/// A tracker that replays a script instead of watching a camera.
///
/// Entry `(at, frame)` becomes visible once the poll's `now` reaches `at` (stage seconds from 0), stamped with
/// `time = at`. A poll returns only the newest visible entry, so entries a slow poller skipped are dropped, as the
/// real mailbox drops them. When no tracked entry has arrived for [`TrackerConfig::lost_after`], one
/// [`TrackFrame::lost`] is returned (at the last frame's time plus `lost_after`, with its `seq`, since no new
/// camera frame exists) — the same signal a real tracker gives when its camera stalls.
///
/// [`latency`](Tracker::latency) counts frames **published**, as a real tracker's `frames` does: every scripted entry
/// once it is due, and every lost frame, including ones a slow poller never sees.
pub struct FakeTracker {
    script: Arc<[(Duration, TrackFrame)]>,
    running: Option<Arc<AtomicBool>>,
    published: Arc<AtomicU64>,
}

impl FakeTracker {
    /// A tracker over `script`, sorted by time (stably, so equal times keep their order).
    pub fn scripted(mut script: Vec<(Duration, TrackFrame)>) -> Self {
        script.sort_by_key(|(at, _)| *at);
        Self { script: script.into(), running: None, published: Arc::new(AtomicU64::new(0)) }
    }
}

impl Tracker for FakeTracker {
    fn devices(&self) -> Vec<CameraInfo> {
        let format = CameraFormat::new(640, 480, 30, PixelFormat::Rgb8);
        vec![CameraInfo::new(FakeCamera::id(), "Fake tracker", vec![format])]
    }

    fn start(&mut self, device: &CameraId, config: &TrackerConfig) -> Result<Box<dyn FrameSource>, TrackError> {
        // Starting again ends the previous session even when the new start is refused.
        self.stop();
        if *device != FakeCamera::id() {
            return Err(TrackError::NoDevice(device.clone()));
        }
        if config.camera.pick(&self.devices()[0].formats).is_none() {
            return Err(TrackError::FormatUnsupported(config.camera));
        }
        let running = Arc::new(AtomicBool::new(true));
        self.running = Some(Arc::clone(&running));
        self.published.store(0, Ordering::Relaxed);
        Ok(Box::new(ScriptedSource {
            script: Arc::clone(&self.script),
            next: 0,
            lost_after: config.lost_after.as_secs_f64(),
            last: None,
            lost_sent: false,
            running,
            published: Arc::clone(&self.published),
        }))
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.store(false, Ordering::Release);
        }
    }

    fn latency(&self) -> LatencyStats {
        let zero = Duration::ZERO;
        LatencyStats::new(self.published.load(Ordering::Relaxed), 0, zero, zero, zero)
    }
}

struct ScriptedSource {
    script: Arc<[(Duration, TrackFrame)]>,
    /// The first entry not yet released.
    next: usize,
    lost_after: f64,
    /// The last tracked frame released.
    last: Option<TrackFrame>,
    /// Whether the current gap has had its lost frame.
    lost_sent: bool,
    running: Arc<AtomicBool>,
    published: Arc<AtomicU64>,
}

impl FrameSource for ScriptedSource {
    fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
        if !self.running.load(Ordering::Acquire) {
            return None;
        }
        // Replay everything due since the last poll in time order, as a tracker would have published it; the
        // mailbox keeps only the newest.
        let mut newest = None;
        let mut published = 0;
        while let Some(&(at, frame)) = self.script.get(self.next).filter(|(at, _)| at.as_secs_f64() <= now.0) {
            self.next += 1;
            let at = at.as_secs_f64();
            // A gap before this entry that was long enough for a lost frame (strictly: an entry arriving exactly
            // when the lost frame falls due is published instead of it).
            if self.lost_due().is_some_and(|due| due < at) {
                self.lost_sent = true;
                published += 1;
            }
            let frame = TrackFrame { time: Seconds(at), ..frame };
            self.lost_sent = frame.status == TrackStatus::Lost;
            if !self.lost_sent {
                self.last = Some(frame);
            }
            published += 1;
            newest = Some(frame);
        }
        // A lost frame already due is newer than any entry replayed above (it falls `lost_after` after the newest
        // tracked one), so it wins, as it would in a real tracker's mailbox.
        if let Some(last) = self.last.filter(|_| self.lost_due().is_some_and(|due| due <= now.0)) {
            self.lost_sent = true;
            published += 1;
            newest = Some(TrackFrame::lost(last.seq, Seconds(last.time.0 + self.lost_after), last.profile));
        }
        self.published.fetch_add(published, Ordering::Relaxed);
        newest
    }
}

impl ScriptedSource {
    /// When the current gap's lost frame falls due, or `None` if it has been sent or nothing was tracked yet.
    fn lost_due(&self) -> Option<f64> {
        self.last.filter(|_| !self.lost_sent).map(|last| last.time.0 + self.lost_after)
    }
}
