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
    /// A `measured` or `scaled` run did not state the provenance spec §10 requires of a gating number.
    #[error("class `{class}` needs provenance (spec §10): {missing} not given")]
    Provenance {
        /// The class that was asked for.
        class: String,
        /// The missing items, as the names of their `OMUZAKI_BUDGET_*` variables.
        missing: String,
    },
    /// A strict `relative` run had nothing it may fail on: no previous report from the same machine, or none of the
    /// compared metrics has a §10 budget.
    #[error("a strict `relative` run needs a previous report from the same machine with a budgeted metric to compare")]
    NoBaseline,
    /// A strict `measured` or `scaled` run had no metric with a §10 budget that applies to its tier, so it judged nothing.
    #[error("a strict `{0}` run needs a metric with a §10 budget for the tier it claims, and found none")]
    NothingToJudge(String),
    /// A strict `relative` run could not show that both reports came from alternating runs of one session.
    #[error("a strict `relative` run needs the same OMUZAKI_BUDGET_SESSION in this run and in the previous report")]
    NoSession,
    /// A measurement class name was not one of `reported`, `measured`, `scaled`, `relative`.
    #[error("unknown measurement class `{0}`")]
    Class(String),
}
