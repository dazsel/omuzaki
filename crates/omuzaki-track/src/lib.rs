//! The webcam face tracker (spec v3 §22 PF-01): MediaPipe's face landmarker (BlazeFace detector, Face Mesh V2,
//! Blendshape V2) with its own video-mode tracking, landmark smoothing and head-pose geometry, running over
//! [`omuzaki_ai_api::Session`]s. Every constant and formula that is not obvious from the models is taken from MediaPipe's
//! C++ graphs (Apache-2.0) and named after the calculator it comes from, so the port can be audited against the
//! source. Ported from `spikes/webcam-track`.
//!
//! | Module | What it does |
//! |---|---|
//! | [`frame`] | [`RgbFrame`] and the rotated-ROI warp into a model's input tensor, with MediaPipe's rounding |
//! | [`roi`] | rotated rectangles, letterboxing and the maths between tensor, ROI and frame coordinates |
//! | [`detector`] | BlazeFace: anchors, decoding, weighted non-max suppression, the face ROI |
//! | [`landmarks`] | Face Mesh V2: the crop, the 478 landmarks, the next frame's ROI |
//! | [`blendshapes`] | Blendshape V2: the landmark subset in, 52 scores out |
//! | [`metadata`] | the canonical face ([`GeometryMetadata`]) and the protobuf reader for it |
//! | [`geometry`] | the head-pose fit (Procrustes through a virtual camera) |
//! | [`guided`] | [`guided::GuidedCalibrator`]: the guided calibration's prompts fitted into per-channel ranges (PF-20) |
//! | [`smoothing`] | the One-Euro landmark filter |
//! | [`models`] | [`models::ModelDir`]: the verified model folder and its three loaded graphs |
//! | [`provision`] | the app's face tracker: the shipped model folder through the runtime it is made for |
//! | [`task`] | the official `face_landmarker.task`, pinned, unpacked into a TFLite model folder |
//! | [`gesture`] | face gestures (tongue out, a wink, puffed cheeks, a wide mouth) that press trigger keys (PF-25) |
//! | [`metrics`] | the tracker section of the live metrics: a 10 Hz tick copying `Tracker::latency` into its writer |
//! | [`doctor`] | the tracking doctor's image readings (light, backlight, glare, the frame edge) over track-api's `Doctor` |
//! | [`pipeline`] | [`FaceTracker`]: the three graphs, tracking between frames, [`Mode`] |
//! | [`sample`] | [`FaceSample`], [`StageTimings`] and [`to_track_frame`] |
//! | [`error`] | [`FaceTrackError`] |
//! | [`webcam`] | [`webcam::WebcamTracker`]: the camera thread, pixel conversion and the preview tap |
//!
//! Parity with MediaPipe is a separate piece.
#![deny(missing_docs)]
// Product names (MediaPipe, BlazeFace, ARKit) are prose here, not code.
#![allow(clippy::doc_markdown)]

pub mod blendshapes;
pub mod body;
pub mod calibration;
pub mod detector;
pub mod doctor;
pub mod error;
pub mod frame;
pub mod geometry;
pub mod gesture;
pub mod guided;
pub mod landmarks;
pub mod metadata;
pub mod metrics;
pub mod models;
pub mod pipeline;
pub mod provision;
pub mod roi;
pub mod sample;
pub mod smoothing;
pub mod ssd;
pub mod task;
pub mod webcam;

pub use blendshapes::blendshapes_signature;
pub use detector::detector_signature;
pub use error::FaceTrackError;
pub use frame::RgbFrame;
pub use landmarks::landmarks_signature;
pub use metadata::GeometryMetadata;
pub use pipeline::{FaceGraphs, FaceTracker, FaceTrackerConfig, Mode};
pub use roi::NormRect;
pub use sample::{FaceSample, StageTimings, to_track_frame};

omuzaki_core::define_link!();
