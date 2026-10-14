//! The whole tracker: detector, landmarks, blendshapes and head pose, with MediaPipe's video-mode tracking.
//!
//! In [`Mode::Video`] the face found on one frame seeds the next frame's ROI, so the detector only runs when the
//! face is new or lost, and landmarks are One-Euro smoothed before blendshapes and pose are computed (as
//! `FaceLandmarker` does). In [`Mode::Image`] every frame is independent: the detector always runs, nothing is smoothed.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::blendshapes::{BlendshapeModel, Blendshapes};
use crate::detector::{DetectorTimings, FaceDetector};
use crate::error::Result;
use crate::frame::Frame;
use crate::geometry::PoseEstimator;
use crate::landmarks::{Landmark, LandmarkModel, LandmarkTimings, next_frame_rect};
use crate::metadata::GeometryMetadata;
use crate::roi::NormRect;
use crate::runtime::RuntimeOptions;
use crate::smoothing::LandmarkSmoother;

/// File names inside the models directory (see `models/PROVENANCE.md`).
const DETECTOR_FILE: &str = "face_detector.onnx";
const LANDMARKS_FILE: &str = "face_landmarks_detector.onnx";
const BLENDSHAPES_FILE: &str = "face_blendshapes.onnx";
const GEOMETRY_FILE: &str = "geometry_pipeline_metadata_landmarks.binarypb";

/// Whether successive frames belong to one video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Independent images: detect on every frame, no smoothing.
    Image,
    /// A video: track the face between frames and smooth the landmarks.
    Video,
}

/// Everything needed to build a tracker.
#[derive(Debug, Clone)]
pub struct TrackerConfig {
    /// Directory with the three ONNX models and the geometry metadata.
    pub models: PathBuf,
    /// Execution provider and threading.
    pub runtime: RuntimeOptions,
    /// Image or video behaviour.
    pub mode: Mode,
    /// A detection scoring below this is ignored (MediaPipe: `min_face_detection_confidence`).
    pub min_detection_confidence: f32,
    /// A landmark result whose presence score is below this counts as "no face"
    /// (MediaPipe: `min_face_presence_confidence`).
    pub min_presence_confidence: f32,
}

/// What the tracker reports for one frame.
#[derive(Debug, Clone)]
pub struct FaceResult {
    /// 478 landmarks in normalised frame coordinates (smoothed in video mode).
    pub landmarks: Vec<Landmark>,
    /// The 51 ARKit blendshape coefficients.
    pub blendshapes: Blendshapes,
    /// Canonical-to-camera head pose, row-major 4x4 (translation in elements 3, 7, 11).
    pub pose: [f32; 16],
    /// Face presence probability from the landmark model.
    pub presence: f32,
    /// The ROI the landmark model was run on (from the detector, or tracked from the previous frame).
    pub roi: NormRect,
}

/// Seconds spent per stage on one frame. Stages that did not run are zero.
#[derive(Debug, Clone, Copy, Default)]
pub struct StageTimings {
    /// Whether the detector ran (false when tracking from the previous frame).
    pub detector_ran: bool,
    /// Detector stages.
    pub detector: DetectorTimings,
    /// Landmark stages.
    pub landmarks: LandmarkTimings,
    /// Blendshape inference including its input gather.
    pub blendshapes: f64,
    /// Head pose fit.
    pub pose: f64,
    /// Landmark smoothing.
    pub smoothing: f64,
    /// The whole call.
    pub total: f64,
}

/// The face tracker. Holds all models, buffers and tracking state; one per camera.
pub struct FaceTracker {
    detector: FaceDetector,
    landmarks: LandmarkModel,
    blendshapes: BlendshapeModel,
    pose: PoseEstimator,
    smoother: LandmarkSmoother,
    mode: Mode,
    min_presence: f32,
    next_rect: Option<NormRect>,
}

impl FaceTracker {
    /// Loads every model and the geometry metadata from `config.models`.
    ///
    /// # Errors
    /// Returns an error when a file is missing or invalid or the backend cannot be set up.
    pub fn new(config: &TrackerConfig) -> Result<Self> {
        let dir: &Path = &config.models;
        let metadata = GeometryMetadata::load(&dir.join(GEOMETRY_FILE))?;
        Ok(Self {
            detector: FaceDetector::load(&dir.join(DETECTOR_FILE), &config.runtime, config.min_detection_confidence)?,
            landmarks: LandmarkModel::load(&dir.join(LANDMARKS_FILE), &config.runtime)?,
            blendshapes: BlendshapeModel::load(&dir.join(BLENDSHAPES_FILE), &config.runtime)?,
            pose: PoseEstimator::new(&metadata),
            smoother: LandmarkSmoother::new(),
            mode: config.mode,
            min_presence: config.min_presence_confidence,
            next_rect: None,
        })
    }

    /// Tracks the face in `frame`, captured `timestamp_ms` after the start of the stream (strictly increasing in
    /// video mode). Returns `None` when no face is found.
    ///
    /// # Errors
    /// Returns an error when inference or the pose fit fails; "no face" is not an error.
    pub fn track(&mut self, frame: &Frame, timestamp_ms: i64) -> Result<(Option<FaceResult>, StageTimings)> {
        let started = Instant::now();
        let mut timings = StageTimings::default();
        let tracked_rect = if self.mode == Mode::Video { self.next_rect.take() } else { None };
        let rect = if tracked_rect.is_some() {
            tracked_rect
        } else {
            timings.detector_ran = true;
            self.detector.detect(frame, &mut timings.detector)?
        };
        let Some(rect) = rect else {
            timings.total = started.elapsed().as_secs_f64();
            return Ok((None, timings));
        };
        let found = self.landmarks.run(frame, &rect, &mut timings.landmarks)?;
        if found.presence < self.min_presence {
            // Tracking lost: this frame reports no face and the next one runs the detector (next_rect stays None).
            timings.total = started.elapsed().as_secs_f64();
            return Ok((None, timings));
        }
        if self.mode == Mode::Video {
            self.next_rect = next_frame_rect(&found.points, frame.width(), frame.height());
        }
        let mut points = found.points;
        let t = Instant::now();
        if self.mode == Mode::Video {
            self.smoother.apply(&mut points, frame.width(), frame.height(), timestamp_ms);
        }
        timings.smoothing = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let blendshapes = self.blendshapes.run(&points, frame.width(), frame.height())?;
        timings.blendshapes = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let pose = self.pose.estimate(&points, frame.width(), frame.height())?;
        timings.pose = t.elapsed().as_secs_f64();
        timings.total = started.elapsed().as_secs_f64();
        Ok((Some(FaceResult { landmarks: points, blendshapes, pose, presence: found.presence, roi: rect }), timings))
    }

    /// Forgets the tracked face and the smoothing history, e.g. when the camera restarts.
    pub fn reset(&mut self) {
        self.next_rect = None;
        self.smoother.reset();
    }
}
