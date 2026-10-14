//! The tracker device trait: a camera plus a model, running on its own thread, publishing [`TrackFrame`]s.
//!
//! [`TrackFrame`]: omuzaki_anim_api::TrackFrame

use std::time::Duration;

use omuzaki_anim_api::FrameSource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::camera::{CameraFormatRequest, CameraId, CameraInfo};
use crate::error::TrackError;

/// How much work the tracker does per frame, so a weak machine keeps a steady rate (PF-01). Shown in the health meter.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QualityTier {
    /// Every network on every frame.
    #[default]
    Full,
    /// The tracker's own reduced setting (for example, blendshapes on alternate frames).
    Balanced,
    /// The cheapest setting that still tracks.
    Light,
}

/// What [`Tracker::start`] runs with: a cold, serialisable setting.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrackerConfig {
    /// The camera mode to ask for.
    #[serde(default)]
    pub camera: CameraFormatRequest,
    /// The work per frame.
    #[serde(default)]
    pub tier: QualityTier,
    /// With no frame published for this long (a stalled camera), the tracker publishes one
    /// [`TrackStatus::Lost`](omuzaki_anim_api::TrackStatus::Lost) frame, so the pipeline never holds a stale face.
    #[serde(default = "default_lost_after")]
    pub lost_after: Duration,
}

/// 200 ms: six frames at 30 fps, well inside the retarget's grace period.
fn default_lost_after() -> Duration {
    Duration::from_millis(200)
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self { camera: CameraFormatRequest::default(), tier: QualityTier::default(), lost_after: default_lost_after() }
    }
}

/// Capture-to-publish latency of the running session: from the frame's capture time on the stage clock to the moment
/// it went into the mailbox. Feeds the health meter (PF-26) and the `track.face` budget.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LatencyStats {
    /// Frames published.
    pub frames: u64,
    /// Camera frames skipped because the tracker was still busy (gaps in [`CameraFrame::seq`](crate::CameraFrame)).
    pub dropped: u64,
    /// Median.
    pub p50: Duration,
    /// 95th percentile.
    pub p95: Duration,
    /// Worst seen.
    pub max: Duration,
}

/// A live tracker. It owns its thread and its camera between [`start`](Self::start) and [`stop`](Self::stop).
///
/// Face found or lost is signalled in the frames themselves: a frame where no face is found is a
/// [`TrackFrame::lost`](omuzaki_anim_api::TrackFrame::lost), and so is the one frame after [`TrackerConfig::lost_after`]
/// without any.
pub trait Tracker: Send {
    /// The cameras it can track from. Cold.
    fn devices(&self) -> Vec<CameraInfo>;

    /// Starts tracking from `device` and returns the pipeline's input port, normally a
    /// [`MailboxReader`](crate::MailboxReader). Starting again stops the previous session first, and that session's
    /// source then returns `None` for ever. Cold; person-initiated only (see the crate docs).
    ///
    /// # Errors
    /// Whatever [`Camera::open`](crate::Camera::open) returns, or [`TrackError::Backend`] if the model cannot load.
    fn start(&mut self, device: &CameraId, config: &TrackerConfig) -> Result<Box<dyn FrameSource>, TrackError>;

    /// Stops the session and releases the camera; a no-op when stopped.
    fn stop(&mut self);

    /// Latency of the current (or last) session. Cheap; the UI may call it every frame.
    fn latency(&self) -> LatencyStats;
}
