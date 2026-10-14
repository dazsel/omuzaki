//! Spike for: MediaPipe's face landmarker (BlazeFace detector, FaceMesh V2, Blendshape V2) running in Rust
//! on ONNX Runtime via `ort`, with MediaPipe's own video-mode tracking, landmark smoothing and head-pose geometry.
//!
//! Every constant and formula that is not obvious from the models is taken from MediaPipe's C++ graphs (Apache-2.0)
//! and named after the calculator it comes from, so the port can be audited against the source.

pub mod blendshapes;
pub mod detector;
pub mod error;
pub mod frame;
pub mod geometry;
pub mod landmarks;
pub mod metadata;
pub mod pipeline;
pub mod roi;
pub mod runtime;
pub mod smoothing;
pub mod timing;
