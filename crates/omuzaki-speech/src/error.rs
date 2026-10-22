//! One error type for the whole crate: every fallible call returns it, nothing panics.

use std::path::PathBuf;

/// What can go wrong in the speech stack.
#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    /// A model or library file is not where the caller said it would be. Models are never committed or bundled; the
    /// message names the catalogue entry that provides the file.
    #[error("{what} not found at {path}; {fetch} (omuzaki_speech::catalog) or fix the path")]
    ModelMissing {
        /// What the file is ("Silero VAD model", "Whisper model", "ONNX Runtime library").
        what: &'static str,
        /// Where it was expected.
        path: PathBuf,
        /// What to fetch: the matching [`crate::catalog::ModelSpec::id`] when the file name is a catalogue name, else
        /// the kind of entry that would do.
        fetch: String,
    },
    /// ONNX Runtime failed to run or a model failed to load.
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
    /// A worker thread could not be started.
    #[error("starting the {name} thread: {source}")]
    Spawn {
        /// Which worker.
        name: &'static str,
        /// The operating system's reason.
        source: std::io::Error,
    },
}

/// Session-builder errors carry the half-built builder so callers can retry; we do not, so it is dropped.
impl From<ort::Error<ort::session::builder::SessionBuilder>> for SpeechError {
    fn from(error: ort::Error<ort::session::builder::SessionBuilder>) -> Self {
        Self::Ort(error.into())
    }
}
