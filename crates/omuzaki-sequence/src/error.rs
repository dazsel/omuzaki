//! [`SequenceError`]: what goes wrong reading, checking or rendering a sequence.

use omuzaki_ops_api::OpError;

/// What goes wrong with a sequence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SequenceError {
    /// The sequence breaks a rule; the message names the field.
    #[error("{0}")]
    Invalid(String),
    /// The stage or the renderer failed.
    #[error("rendering: {0}")]
    Render(String),
    /// The encoder or a file failed.
    #[error("writing: {0}")]
    Write(String),
}

impl From<SequenceError> for OpError {
    fn from(error: SequenceError) -> Self {
        match error {
            SequenceError::Invalid(reason) => OpError::invalid("sequence", reason)
                .hint("sequence.kinds lists the easings, layer kinds and volume presets"),
            SequenceError::Render(reason) | SequenceError::Write(reason) => OpError::internal(reason),
        }
    }
}

impl From<omuzaki_short::ShortError> for SequenceError {
    fn from(error: omuzaki_short::ShortError) -> Self {
        Self::Write(error.to_string())
    }
}
