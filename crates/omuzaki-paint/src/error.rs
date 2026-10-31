//! Why a painting can't be read or drawn.

/// Why a painting, a layer or a mark is refused. Each names the field at fault in words an agent can act on.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum PaintError {
    /// A colour that isn't `#rrggbb` or `#rrggbbaa`.
    #[error("{0:?} is not a colour: write #rrggbb or #rrggbbaa")]
    Colour(String),
    /// A field out of its range, or a value that can't be used.
    #[error("{field}: {reason}")]
    Invalid {
        /// The field, as a path (`marks/3/points`).
        field: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A layer id that isn't in the painting.
    #[error("there is no layer {0:?} in the painting")]
    NoLayer(String),
    /// Bucket fills that sample each other in a loop, so neither can be filled first.
    #[error("layers sample each other in a loop: {0}")]
    SampleLoop(String),
    /// A picture that can't be encoded.
    #[error("encoding the picture failed: {0}")]
    Encode(String),
}

impl PaintError {
    /// An [`Invalid`](Self::Invalid) error.
    pub fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid { field: field.into(), reason: reason.into() }
    }
}
