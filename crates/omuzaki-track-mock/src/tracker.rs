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
pub struct FakeTracker {
    script: Arc<[(Duration, TrackFrame)]>,
    running: Option<Arc<AtomicBool>>,
    delivered: Arc<AtomicU64>,
}

impl FakeTracker {
    /// A tracker over `script`, sorted by time (stably, so equal times keep their order).
    pub fn scripted(mut script: Vec<(Duration, TrackFrame)>) -> Self {
        script.sort_by_key(|(at, _)| *at);
        Self { script: script.into(), running: None, delivered: Arc::new(AtomicU64::new(0)) }
    }
}

impl Tracker for FakeTracker {
    fn devices(&self) -> Vec<CameraInfo> {
        let format = CameraFormat { width: 640, height: 480, fps: 30, pixel: PixelFormat::Rgb8 };
        vec![CameraInfo { id: FakeCamera::id(), name: "Fake tracker".to_owned(), formats: vec![format] }]
    }

    fn start(&mut self, device: &CameraId, config: &TrackerConfig) -> Result<Box<dyn FrameSource>, TrackError> {
        if *device != FakeCamera::id() {
            return Err(TrackError::NoDevice(device.clone()));
        }
        self.stop();
        let running = Arc::new(AtomicBool::new(true));
        self.running = Some(Arc::clone(&running));
        self.delivered.store(0, Ordering::Relaxed);
        Ok(Box::new(ScriptedSource {
            script: Arc::clone(&self.script),
            next: 0,
            lost_after: config.lost_after.as_secs_f64(),
            last: None,
            lost_sent: false,
            running,
            delivered: Arc::clone(&self.delivered),
        }))
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.store(false, Ordering::Release);
        }
    }

    fn latency(&self) -> LatencyStats {
        LatencyStats { frames: self.delivered.load(Ordering::Relaxed), ..LatencyStats::default() }
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
    delivered: Arc<AtomicU64>,
}

impl FrameSource for ScriptedSource {
    fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
        if !self.running.load(Ordering::Acquire) {
            return None;
        }
        let visible = self.script[self.next..].iter().take_while(|(at, _)| at.as_secs_f64() <= now.0).count();
        let frame = if visible > 0 {
            self.next += visible;
            let (at, frame) = self.script[self.next - 1];
            let frame = TrackFrame { time: Seconds(at.as_secs_f64()), ..frame };
            self.lost_sent = frame.status == TrackStatus::Lost;
            if !self.lost_sent {
                self.last = Some(frame);
            }
            frame
        } else {
            let last = self.last.filter(|last| !self.lost_sent && now.0 - last.time.0 >= self.lost_after)?;
            self.lost_sent = true;
            TrackFrame::lost(last.seq, Seconds(last.time.0 + self.lost_after), last.profile)
        };
        self.delivered.fetch_add(1, Ordering::Relaxed);
        Some(frame)
    }
}
