//! [`ShortError`]: what goes wrong reading, planning or rendering a short.

use omuzaki_ops_api::OpError;

/// What goes wrong with a short.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShortError {
    /// The text is not a short file.
    #[error("the short file doesn't read: {0}")]
    Parse(String),
    /// A value breaks a rule.
    #[error("{field}: {reason}")]
    Invalid {
        /// Where, as a path into the short (`line[3].camera`).
        field: String,
        /// What rule, in words.
        reason: String,
    },
    /// The voice can't be read or matched to the lines.
    #[error("the voice: {0}")]
    Voice(String),
    /// The stage or the renderer failed.
    #[error("rendering: {0}")]
    Render(String),
    /// The encoder or the file failed.
    #[error("writing the video: {0}")]
    Encode(String),
}

impl ShortError {
    /// A value at `field` that breaks a rule.
    pub fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid { field: field.into(), reason: reason.into() }
    }

    /// The same error placed under `field` (`camera` becomes `line[3].camera`).
    #[must_use]
    pub fn at(self, field: String) -> Self {
        match self {
            Self::Invalid { reason, .. } => Self::Invalid { field, reason },
            other => other,
        }
    }
}

impl From<ShortError> for OpError {
    fn from(error: ShortError) -> Self {
        match error {
            ShortError::Parse(reason) => OpError::invalid("short", reason).hint("check the short file's TOML"),
            ShortError::Invalid { field, reason } => OpError::invalid(field, reason),
            ShortError::Voice(reason) => OpError::invalid("voice", reason),
            ShortError::Render(reason) | ShortError::Encode(reason) => OpError::internal(reason),
        }
    }
}
