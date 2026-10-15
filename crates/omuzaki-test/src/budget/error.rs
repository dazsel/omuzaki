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
    /// A report was written in a layout this harness does not read.
    #[error("report schema {found} is not supported (this harness reads schema {supported})")]
    Schema {
        /// The version found in the file.
        found: u32,
        /// The version this harness reads.
        supported: u32,
    },
    /// A gating run (class other than `reported`) had violations.
    #[error("{0}")]
    Violations(String),
    /// A measurement class name was not one of `reported`, `measured`, `scaled`, `relative`.
    #[error("unknown measurement class `{0}`")]
    Class(String),
}
