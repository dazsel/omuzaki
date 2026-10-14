//! Why an import or export failed. Warnings and lossy-conversion notes are not errors: they travel in the
//! [`Report`](crate::Report) of a successful outcome.

use crate::Report;

/// A failed import or export.
///
/// The op layer maps these onto `omuzaki_ops_api::OpError`: `Malformed`, `Limit` and `Rejected` become `Invalid`,
/// `Unsupported` stays `Unsupported`, `Resource` becomes `NotFound`, and `Internal` stays `Internal`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IoError {
    /// The bytes aren't the format they claim to be (bad magic, truncated chunk, invalid JSON).
    #[error("malformed file: {message}")]
    Malformed {
        /// What is wrong.
        message: String,
        /// Where, as a byte offset or JSON pointer, when known.
        at: Option<String>,
    },
    /// Valid, but uses something this importer or exporter doesn't handle (a required extension, sparse accessors).
    #[error("unsupported: {what}")]
    Unsupported {
        /// What isn't supported.
        what: String,
        /// What to do instead.
        hint: Option<String>,
    },
    /// A file the glTF refers to (an external `.bin` or image) couldn't be read.
    #[error("couldn't read {uri}: {reason}")]
    Resource {
        /// The URI as written in the glTF.
        uri: String,
        /// Why it failed.
        reason: String,
    },
    /// The input is bigger than the configured [`Limits`](crate::Limits).
    #[error("{what} is over the limit: {got} > {limit}")]
    Limit {
        /// Which limit.
        what: &'static str,
        /// The configured maximum.
        limit: u64,
        /// What the input has.
        got: u64,
    },
    /// The checks found errors; nothing was imported or written. The report lists them all.
    #[error("rejected with {} error(s); see the report", .0.errors().count())]
    Rejected(Report),
    /// The caller cancelled through the [`Progress`](crate::Progress) handle.
    #[error("cancelled")]
    Cancelled,
    /// A bug in Omuzaki. Report it.
    #[error("internal error: {0}")]
    Internal(String),
}

impl IoError {
    /// A [`IoError::Malformed`] without a location.
    pub fn malformed(message: impl Into<String>) -> Self {
        Self::Malformed { message: message.into(), at: None }
    }

    /// A [`IoError::Unsupported`] with a hint.
    pub fn unsupported(what: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::Unsupported { what: what.into(), hint: Some(hint.into()) }
    }
}
