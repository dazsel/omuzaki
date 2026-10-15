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

/// What [`Tracker::start`] runs with: a cold, serialisable setting. Built from [`Default`] and the `with_*` methods,
/// so a new field edits only this crate (ADR 0054 §4).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct TrackerConfig {
    /// The camera mode to ask for.
    #[serde(default)]
    pub camera: CameraFormatRequest,
    /// The work per frame.
    #[serde(default)]
    pub tier: QualityTier,
    /// With no frame published for this long (a stalled camera), the tracker publishes one
    /// [`TrackStatus::Lost`](omuzaki_anim_api::TrackStatus::Lost) frame, so the pipeline never holds a stale face.
    /// Saved as whole milliseconds, `lost_after_ms`, at least 1: a person edits it. A value set here is saved rounded
    /// up to whole milliseconds, at least 1 ms; [`with_lost_after`](Self::with_lost_after) normalises it the same way.
    #[serde(rename = "lost_after_ms", with = "whole_millis", default = "default_lost_after")]
    #[schemars(with = "u64", range(min = 1))]
    pub lost_after: Duration,
}

/// A [`Duration`] saved as whole milliseconds, rounded up, at least 1, so every value saves to one that reads back.
mod whole_millis {
    use std::time::Duration;

    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(ceil_millis(*value))
    }

    /// Whole milliseconds, rounded up, at least 1.
    pub(super) fn ceil_millis(value: Duration) -> u64 {
        let ms = value.as_nanos().div_ceil(1_000_000).max(1);
        u64::try_from(ms).unwrap_or(u64::MAX)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        match u64::deserialize(deserializer)? {
            0 => Err(D::Error::custom("lost_after_ms must be at least 1")),
            ms => Ok(Duration::from_millis(ms)),
        }
    }
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

impl TrackerConfig {
    /// With this camera mode request.
    #[must_use]
    pub fn with_camera(mut self, camera: CameraFormatRequest) -> Self {
        self.camera = camera;
        self
    }

    /// With this work per frame.
    #[must_use]
    pub fn with_tier(mut self, tier: QualityTier) -> Self {
        self.tier = tier;
        self
    }

    /// With this stall time before a lost frame, rounded up to whole milliseconds and at least 1 ms, as it is saved
    /// (`Duration::ZERO` gives 1 ms, 1.5 ms gives 2 ms).
    #[must_use]
    pub fn with_lost_after(mut self, lost_after: Duration) -> Self {
        self.lost_after = Duration::from_millis(whole_millis::ceil_millis(lost_after));
        self
    }
}

/// Capture-to-publish latency of the running session: from the frame's capture time on the stage clock to the moment
/// it went into the mailbox. Feeds the health meter (PF-26) and the `track.face` budget. Built from [`Default`] or
/// [`new`](Self::new) (ADR 0054 §4).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LatencyStats {
    /// Frames published.
    pub frames: u64,
    /// Camera frames skipped because the tracker was still busy (gaps in [`CameraFrame::seq`](crate::camera::CameraFrame::seq)).
    pub dropped: u64,
    /// Median.
    pub p50: Duration,
    /// 95th percentile.
    pub p95: Duration,
    /// Worst seen.
    pub max: Duration,
}

impl LatencyStats {
    /// `frames` published and `dropped` skipped, with the median, 95th percentile and worst latency.
    pub const fn new(frames: u64, dropped: u64, p50: Duration, p95: Duration, max: Duration) -> Self {
        Self { frames, dropped, p50, p95, max }
    }
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
    /// [`MailboxReader`](crate::mailbox::MailboxReader). Starting again stops the previous session first, and that session's
    /// source then returns `None` for ever. Cold; person-initiated only (see the crate docs).
    ///
    /// # Errors
    /// Whatever [`Camera::open`](crate::camera::Camera::open) returns, or [`TrackError::Backend`] if the model cannot load.
    fn start(&mut self, device: &CameraId, config: &TrackerConfig) -> Result<Box<dyn FrameSource>, TrackError>;

    /// Stops the session and releases the camera; a no-op when stopped.
    fn stop(&mut self);

    /// Latency of the current (or last) session. Cheap; the UI may call it every frame.
    fn latency(&self) -> LatencyStats;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_methods_set_one_field_each() {
        let camera = CameraFormatRequest::new(1280, 720, 30);
        let config = TrackerConfig::default()
            .with_camera(camera)
            .with_tier(QualityTier::Light)
            .with_lost_after(Duration::from_millis(50));
        assert_eq!(config.camera, camera);
        assert_eq!(config.tier, QualityTier::Light);
        assert_eq!(config.lost_after, Duration::from_millis(50));
        assert_eq!(TrackerConfig::default().lost_after, Duration::from_millis(200));
    }

    #[test]
    fn lost_after_is_saved_as_whole_milliseconds() {
        let json = serde_json::to_value(TrackerConfig::default()).unwrap();
        assert_eq!(json["lost_after_ms"], 200);
        assert!(json.get("lost_after").is_none());
        let back: TrackerConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, TrackerConfig::default());

        let read: TrackerConfig = serde_json::from_str(r#"{"lost_after_ms": 350}"#).unwrap();
        assert_eq!(read.lost_after, Duration::from_millis(350));
        assert_eq!(serde_json::from_str::<TrackerConfig>("{}").unwrap(), TrackerConfig::default());
        assert!(serde_json::from_str::<TrackerConfig>(r#"{"lost_after_ms": 0}"#).is_err());
        assert!(serde_json::from_str::<TrackerConfig>(r#"{"lost_after_ms": 0.5}"#).is_err());
    }

    #[test]
    fn with_lost_after_rounds_up_to_a_millisecond_that_round_trips() {
        for (given, want) in [(Duration::ZERO, 1), (Duration::from_micros(1500), 2), (Duration::from_millis(7), 7)] {
            let config = TrackerConfig::default().with_lost_after(given);
            assert_eq!(config.lost_after, Duration::from_millis(want), "{given:?}");
            let json = serde_json::to_value(config).unwrap();
            assert_eq!(json["lost_after_ms"], want, "{given:?}");
            assert_eq!(serde_json::from_value::<TrackerConfig>(json).unwrap(), config, "{given:?}");
        }
        // A value set on the field directly still saves to one that reads back.
        let raw = TrackerConfig { lost_after: Duration::ZERO, ..TrackerConfig::default() };
        assert_eq!(serde_json::to_value(raw).unwrap()["lost_after_ms"], 1);
    }

    #[test]
    fn the_schema_shows_lost_after_ms_as_an_integer_of_at_least_1() {
        let schema = serde_json::to_value(schemars::schema_for!(TrackerConfig)).unwrap();
        let field = &schema["properties"]["lost_after_ms"];
        assert_eq!(field["type"], "integer", "{field}");
        assert_eq!(field["minimum"], 1, "{field}");
        assert_eq!(field["default"], 200, "{field}");
    }
}
