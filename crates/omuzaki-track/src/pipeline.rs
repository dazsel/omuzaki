//! The whole tracker: detector, landmarks, blendshapes and head pose, with MediaPipe's video-mode tracking.
//!
//! In [`Mode::Video`] the face found on one frame seeds the next frame's ROI, so the detector only runs when the
//! face is new or lost, and landmarks are One-Euro smoothed before blendshapes and pose are computed (as
//! `FaceLandmarker` does). In [`Mode::Image`] every frame is independent: the detector always runs, nothing is smoothed.
//!
//! On top of that, a video-mode frame may be skipped (the last result stands) when the ROI barely changed, and the
//! detector re-runs on a timer; see [`ReuseConfig`].

use std::time::Instant;

use omuzaki_ai_api::Session;
use omuzaki_anim_api::{ARKIT_COUNT, Channel, ChannelMask};
use omuzaki_core::time::Seconds;

use crate::blendshapes::BlendshapeModel;
use crate::detector::FaceDetector;
use crate::error::{FaceTrackError, Result};
use crate::frame::RgbFrame;
use crate::geometry::PoseEstimator;
use crate::landmarks::{Landmark, LandmarkModel, NUM_LANDMARKS, next_frame_rect};
use crate::metadata::GeometryMetadata;
use crate::roi::NormRect;
use crate::sample::{FaceSample, StageTimings, coefficient_channels};
use crate::smoothing::LandmarkSmoother;

mod reuse;
pub use reuse::ReuseConfig;
use reuse::{Plan, ReuseGate};

/// Whether successive frames belong to one video.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Independent images: detect on every frame, no smoothing.
    Image,
    /// A video: track the face between frames and smooth the landmarks.
    Video,
}

/// Everything needed to build a tracker besides the graphs.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FaceTrackerConfig {
    /// Image or video behaviour.
    pub mode: Mode,
    /// A detection scoring below this is ignored (MediaPipe: `min_face_detection_confidence`).
    pub min_detection: f32,
    /// A landmark result whose presence score is below this counts as "no face"
    /// (MediaPipe: `min_face_presence_confidence`).
    pub min_presence: f32,
    /// ROI reuse and frame skipping in video mode.
    pub reuse: ReuseConfig,
}

impl Default for FaceTrackerConfig {
    /// Video mode with MediaPipe's thresholds (0.5 for detection and for presence) and the default [`ReuseConfig`].
    fn default() -> Self {
        Self { mode: Mode::Video, min_detection: 0.5, min_presence: 0.5, reuse: ReuseConfig::default() }
    }
}

/// The three graphs, as sessions of the slot signatures [`detector_signature`](crate::detector_signature),
/// [`landmarks_signature`](crate::landmarks_signature) and [`blendshapes_signature`](crate::blendshapes_signature).
pub struct FaceGraphs {
    /// BlazeFace short-range: a 128x128 image to 896 anchors.
    pub detector: Box<dyn Session>,
    /// Face Mesh V2: a 256x256 face crop to 478 landmarks and a presence logit.
    pub landmarks: Box<dyn Session>,
    /// Blendshape V2: 146 landmarks to 52 scores.
    pub blendshapes: Box<dyn Session>,
}

/// Why a frame has no face, for the paths that must tell the reasons apart.
enum Lost {
    /// The ROI had no area (a broken model output or a bad caller).
    NoArea,
    /// The landmark model's presence score was under the threshold.
    LowPresence,
    /// The landmarks were too compact or degenerate to fit a pose to.
    NoPose,
}

/// The face tracker. Holds all sessions, buffers and tracking state; one per camera.
///
/// **Hot path.** [`track`](Self::track) allocates nothing once the sessions' output buffers have grown (the first
/// frame); everything it needs is preallocated here.
pub struct FaceTracker {
    detector: FaceDetector,
    landmarks: LandmarkModel,
    blendshapes: BlendshapeModel,
    pose: PoseEstimator,
    smoother: LandmarkSmoother,
    mode: Mode,
    reuse_enabled: bool,
    min_presence: f32,
    next_rect: Option<NormRect>,
    reuse: ReuseGate,
    /// The last full pass's result, which a skipped frame repeats. Kept only while frames may be skipped.
    last: Option<FaceSample>,
    /// The landmarks of the frame being tracked.
    points: [Landmark; NUM_LANDMARKS],
    /// The `ARKit` channel of each of the model's 51 coefficients.
    channels: [Channel; ARKIT_COUNT - 1],
    /// The channels the tracker measures.
    present: ChannelMask,
}

impl FaceTracker {
    /// Builds a tracker over three graph sessions and the canonical-face metadata.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`] when a session's signature is not its graph's slot signature.
    pub fn new(graphs: FaceGraphs, geometry: &GeometryMetadata, config: FaceTrackerConfig) -> Result<Self> {
        let channels = coefficient_channels().ok_or(FaceTrackError::Shape { graph: "blendshapes" })?;
        let mut present = ChannelMask::EMPTY;
        for channel in channels {
            present.insert(channel);
        }
        Ok(Self {
            detector: FaceDetector::new(graphs.detector, config.min_detection)?,
            landmarks: LandmarkModel::new(graphs.landmarks)?,
            blendshapes: BlendshapeModel::new(graphs.blendshapes)?,
            pose: PoseEstimator::new(geometry),
            smoother: LandmarkSmoother::new(),
            mode: config.mode,
            reuse_enabled: config.reuse.enabled,
            min_presence: config.min_presence,
            next_rect: None,
            reuse: ReuseGate::new(config.reuse),
            last: None,
            points: [[0.0; 3]; NUM_LANDMARKS],
            channels,
            present,
        })
    }

    /// Tracks the face in `frame`, captured at `time` (strictly increasing in video mode). Returns `None` when no
    /// face is found; that is not an error.
    ///
    /// # Errors
    /// [`FaceTrackError::BadFrame`] for an unusable frame, [`FaceTrackError::Session`] when a graph fails to run and
    /// [`FaceTrackError::Shape`] when one answers with the wrong number of values.
    pub fn track(&mut self, frame: &RgbFrame<'_>, time: Seconds) -> Result<(Option<FaceSample>, StageTimings)> {
        frame.validate()?;
        let timings = StageTimings::default();
        let video = self.mode == Mode::Video;
        let plan = if video { self.reuse.plan(frame, self.next_rect.as_ref()) } else { Plan::Detect };
        if plan == Plan::Skip {
            self.reuse.skipped();
            debug_assert!(self.last.is_some(), "a skip needs the last result");
            return Ok((self.last.clone(), timings));
        }
        let result = self.full_pass(frame, time, plan);
        if result.is_err() {
            // A graph failed part-way: whatever it left (the ROI for the next frame, a reference taken before it) is
            // unproven, so the next frame detects instead of repeating the old sample.
            self.lose_face();
        }
        result
    }

    /// The detector (when `plan` asks for it), then the landmark pass and what follows it, for a video or image frame.
    fn full_pass(
        &mut self,
        frame: &RgbFrame<'_>,
        time: Seconds,
        plan: Plan,
    ) -> Result<(Option<FaceSample>, StageTimings)> {
        let mut timings = StageTimings::default();
        let video = self.mode == Mode::Video;
        let tracked = if video { self.next_rect.take() } else { None };
        let detected = plan == Plan::Detect;
        let rect = if detected {
            let started = Instant::now();
            // A timed re-detection that misses keeps the face the landmarks already follow.
            let found = self.detector.detect(frame)?.or(tracked);
            timings.detect_us = micros_since(started);
            found
        } else {
            tracked
        };
        let Some(rect) = rect else {
            self.forget_pass();
            return Ok((None, timings));
        };
        // A lost face reports nothing, and `next_rect` stays `None` so the next frame runs the detector.
        let sample = self.track_in(frame, rect, time, true, &mut timings)?.ok();
        match &sample {
            Some(found) if video => {
                self.reuse.passed(frame, detected, self.next_rect, found.presence);
                if self.reuse_enabled {
                    self.last = Some(found.clone());
                }
            }
            Some(_) => {}
            None => self.forget_pass(),
        }
        Ok((sample, timings))
    }

    /// Drops the tracked face and what a skipped frame would repeat, so the next frame runs the detector.
    fn lose_face(&mut self) {
        self.next_rect = None;
        self.forget_pass();
    }

    /// Drops what a skipped frame would repeat.
    fn forget_pass(&mut self) {
        self.reuse.lost();
        self.last = None;
    }

    /// Forgets the tracked face, the smoothing history and whatever the sessions carry, e.g. when the camera restarts.
    pub fn reset(&mut self) {
        self.next_rect = None;
        self.forget_pass();
        self.smoother.reset();
        self.detector.reset();
        self.landmarks.reset();
        self.blendshapes.reset();
    }

    /// Skips detection: tracks the face inside `roi` (parity tests and the model checks). The presence threshold does
    /// not apply, so a weak face is still reported; in video mode the sample seeds the next frame's ROI and the
    /// smoother as usual.
    ///
    /// # Errors
    /// As [`track`](Self::track), plus [`FaceTrackError::Shape`] (graph `"roi"`) for a region with no area and
    /// (graph `"geometry"`) when the landmarks cannot be fitted a pose.
    #[doc(hidden)]
    pub fn track_roi(&mut self, frame: &RgbFrame<'_>, roi: NormRect, time: Seconds) -> Result<FaceSample> {
        frame.validate()?;
        self.forget_pass();
        let mut timings = StageTimings::default();
        let result = self.track_in(frame, roi, time, false, &mut timings).and_then(|tracked| {
            tracked.map_err(|lost| match lost {
                Lost::NoArea => FaceTrackError::Shape { graph: "roi" },
                Lost::LowPresence | Lost::NoPose => FaceTrackError::Shape { graph: "geometry" },
            })
        });
        if result.is_err() {
            self.lose_face();
        }
        result
    }

    /// Landmarks, smoothing, blendshapes and pose inside `rect`. `gate` applies the presence threshold.
    fn track_in(
        &mut self,
        frame: &RgbFrame<'_>,
        rect: NormRect,
        time: Seconds,
        gate: bool,
        timings: &mut StageTimings,
    ) -> Result<std::result::Result<FaceSample, Lost>> {
        let started = Instant::now();
        let presence = self.landmarks.run(frame, &rect, &mut self.points)?;
        timings.landmarks_us = micros_since(started);
        let Some(presence) = presence else { return Ok(Err(Lost::NoArea)) };
        if gate && presence < self.min_presence {
            return Ok(Err(Lost::LowPresence));
        }
        let (width, height) = (frame.width as usize, frame.height as usize);
        if self.mode == Mode::Video {
            // The next ROI comes from the unsmoothed landmarks; smoothing then rewrites them in place.
            self.next_rect = next_frame_rect(&self.points, width, height);
        }
        let started = Instant::now();
        if self.mode == Mode::Video {
            self.smoother.apply(&mut self.points, width, height, nanoseconds(time));
        }
        let smoothing_us = micros_since(started);

        let started = Instant::now();
        let coefficients = self.blendshapes.run(&self.points, frame.width, frame.height)?;
        timings.blendshapes_us = micros_since(started);

        let started = Instant::now();
        let pose = self.pose.estimate(&self.points, width, height);
        timings.geometry_us = smoothing_us.saturating_add(micros_since(started));
        let Some(pose) = pose else {
            self.next_rect = None;
            return Ok(Err(Lost::NoPose));
        };

        let mut blendshapes = [0.0; ARKIT_COUNT];
        for (channel, value) in self.channels.iter().zip(coefficients) {
            blendshapes[channel.index()] = value;
        }
        Ok(Ok(FaceSample { blendshapes, present: self.present, pose, landmarks: self.points, presence, roi: rect }))
    }
}

/// Microseconds since `started`, saturating at `u32::MAX`.
fn micros_since(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX)
}

/// A pipeline time in nanoseconds, the smoother's unit (NaN reads as 0).
#[allow(clippy::cast_possible_truncation)] // seconds * 1e9 is far inside i64 for any real session
fn nanoseconds(time: Seconds) -> i64 {
    (time.0 * 1e9).round() as i64
}
