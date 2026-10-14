//! One error type for the whole spike: every fallible call returns it, nothing panics.

use std::path::PathBuf;

/// What can go wrong in the speech stack.
#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    /// A WAV file could not be read or written.
    #[error("wav file {path}: {source}")]
    Wav {
        /// The file involved.
        path: PathBuf,
        /// The underlying decoder or encoder error.
        source: hound::Error,
    },
    /// A WAV file is not 16 kHz mono, which both models require.
    #[error("wav file {path} is {rate} Hz with {channels} channel(s); the speech stack needs 16000 Hz mono")]
    WavFormat {
        /// The file involved.
        path: PathBuf,
        /// Its sample rate.
        rate: u32,
        /// Its channel count.
        channels: u16,
    },
    /// ONNX Runtime failed to load, or a model failed to run.
    #[error("onnx runtime: {0}")]
    Ort(#[from] ort::Error),
    /// The ONNX Runtime shared library could not be loaded.
    #[error("loading the onnx runtime library: {0}")]
    OrtLoad(#[from] ort::LoadDynamicError),
    /// whisper.cpp failed to load a model or to transcribe.
    #[error("whisper: {0}")]
    Whisper(#[from] whisper_rs::WhisperError),
    /// A model returned a tensor in a shape the wrapper does not expect.
    #[error("unexpected model output: {0}")]
    ModelOutput(String),
    /// A model path contained bytes that are not valid UTF-8 (whisper.cpp takes `&str` paths).
    #[error("model path {0} is not valid UTF-8")]
    PathNotUtf8(PathBuf),
}

/// Session-builder errors carry the half-built builder so callers can retry; the spike does not, so it drops it.
impl From<ort::Error<ort::session::builder::SessionBuilder>> for SpeechError {
    fn from(error: ort::Error<ort::session::builder::SessionBuilder>) -> Self {
        Self::Ort(error.into())
    }
}
