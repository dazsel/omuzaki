//! Errors are values (three standards): nothing in this contract panics on bad input.

/// Why a renderer call failed. Every variant says what to fix; none leaves the renderer in a half-changed state.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// A handle isn't live: it was released, or it came from another renderer.
    #[error("that {what} handle isn't live (released, or issued by another renderer)")]
    UnknownHandle {
        /// The kind of resource: `mesh`, `texture`, `material` or `target`.
        what: &'static str,
    },
    /// The resource is still referenced; release the user first (for example the material before its texture).
    #[error("the {what} is still used by a {by}; release the {by} first")]
    InUse {
        /// The resource that can't be released yet.
        what: &'static str,
        /// What still uses it.
        by: &'static str,
    },
    /// An argument breaks a rule of the contract (a wrong length, an out-of-range index, a non-finite number).
    #[error("invalid {what}: {reason}")]
    Invalid {
        /// What was being checked.
        what: &'static str,
        /// The rule it broke, with the offending value.
        reason: String,
    },
    /// A request exceeds a guaranteed limit or the renderer's [`Capabilities`](crate::Capabilities).
    #[error("{what} is over the limit of {limit}")]
    LimitExceeded {
        /// What exceeded it.
        what: &'static str,
        /// The limit.
        limit: u64,
    },
    /// This renderer doesn't implement the request (a feature, not a bug); check `capabilities()` first.
    #[error("{0} isn't supported by this renderer")]
    Unsupported(&'static str),
    /// The GPU device was lost. Every handle is now invalid: build a new renderer and re-upload from the CPU copies.
    #[error("the GPU device was lost; rebuild the renderer and upload its resources again")]
    DeviceLost,
    /// The GPU ran out of memory creating something.
    #[error("out of GPU memory creating {what}")]
    OutOfMemory {
        /// What was being created.
        what: &'static str,
    },
    /// An error from the backend that this contract can't classify. Text only: no backend types leak (§39 rule 1).
    #[error("{0}")]
    Backend(String),
}

impl RenderError {
    /// Shorthand for [`RenderError::Invalid`].
    pub fn invalid(what: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid { what, reason: reason.into() }
    }
}

impl From<omuzaki_core::mesh::MeshError> for RenderError {
    fn from(e: omuzaki_core::mesh::MeshError) -> Self {
        Self::invalid("mesh", e.0)
    }
}
