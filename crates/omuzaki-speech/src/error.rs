//! One error type for the whole crate: every fallible call returns it, nothing panics.

use omuzaki_ai_api::RunError;

/// What can go wrong in the speech stack.
#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    /// A model session failed to run. [`RunError::Unavailable`] means the slot is switching models: no answer this
    /// time, ask again.
    #[error("the speech model: {0}")]
    Run(#[from] RunError),
    /// A session given for a slot presents another slot's signature.
    #[error("the session given for {0} doesn't present that slot's signature")]
    Signature(&'static str),
    /// A model returned a tensor in a shape the wrapper does not expect.
    #[error("unexpected model output: {0}")]
    ModelOutput(String),
    /// A worker thread could not be started.
    #[error("starting the {name} thread: {source}")]
    Spawn {
        /// Which worker.
        name: &'static str,
        /// The operating system's reason.
        source: std::io::Error,
    },
}
