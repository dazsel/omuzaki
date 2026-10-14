//! One error type for the whole tracker: every fallible step returns a value instead of panicking.

use std::path::PathBuf;

/// What can go wrong while loading models or tracking a frame.
#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    /// ONNX Runtime failed (library load, session creation, inference).
    #[error("onnx runtime: {0}")]
    Ort(#[from] ort::Error),
    /// The ONNX Runtime shared library could not be loaded.
    #[error("loading the onnx runtime library: {0}")]
    OrtLoad(#[from] ort::LoadDynamicError),
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// An image could not be decoded.
    #[error("{path}: {source}")]
    Image {
        /// The image involved.
        path: PathBuf,
        /// The underlying error.
        source: image::ImageError,
    },
    /// A model file or its metadata does not look like the MediaPipe face models this crate expects.
    #[error("unexpected model data: {0}")]
    Model(String),
    /// A pixel buffer, rectangle or landmark set cannot be used (wrong size, zero area, too small to solve).
    #[error("invalid input: {0}")]
    Input(String),
}

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, TrackError>;
