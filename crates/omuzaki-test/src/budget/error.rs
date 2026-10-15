//! Errors of the budget harness.

use std::io;
use std::path::PathBuf;

/// What can go wrong while reading the spec, recording timings or writing a report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BudgetError {
    /// The spec's §10 budget table was not found or is malformed.
    #[error("spec §10 budget table: {0}")]
    Spec(String),
    /// A timing span was registered twice or used with an id that this recorder did not hand out.
    #[error("timing span: {0}")]
    Span(String),
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file concerned.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// A report could not be read back or written as JSON.
    #[error("report JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A budget run was asked to fail on a violation, and there was one.
    #[error("{0}")]
    Violations(String),
}
