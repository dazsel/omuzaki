//! What grading produces: one [`CheckResult`] per check, with an [`Outcome`] that keeps the model's mistakes apart
//! from the harness's (design 5.4).

use omuzaki_ops_api::{ErrorCode, OpError};
use serde::Serialize;
use serde_json::Value;

/// How one check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The result meets the check.
    Pass,
    /// The result does not meet it, or the agent broke or removed what the check measures.
    Fail,
    /// The check could not run for its own reasons: harness trouble, never scored as the model's.
    Error,
}

impl Outcome {
    /// The outcome of an op that failed on the **result** project: `not_found` and `invalid` mean the agent broke or
    /// removed what the check reads, so they are a fail; anything else is the harness's.
    #[must_use]
    pub fn of_result_error(error: &OpError) -> Self {
        match error.code {
            ErrorCode::NotFound | ErrorCode::Invalid => Self::Fail,
            _ => Self::Error,
        }
    }
}

/// What one check found, before it is named and weighed as hard or soft.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    /// How it ended.
    pub outcome: Outcome,
    /// The value on the fixture, when the check looked at one.
    pub before: Option<Value>,
    /// The value on the result, when the check looked at one.
    pub after: Option<Value>,
    /// One line saying why.
    pub detail: String,
}

impl Verdict {
    /// A pass with `detail`.
    pub fn pass(detail: impl Into<String>) -> Self {
        Self { outcome: Outcome::Pass, before: None, after: None, detail: detail.into() }
    }

    /// A fail with `detail`.
    pub fn fail(detail: impl Into<String>) -> Self {
        Self { outcome: Outcome::Fail, ..Self::pass(detail) }
    }

    /// An error with `detail`.
    pub fn error(detail: impl Into<String>) -> Self {
        Self { outcome: Outcome::Error, ..Self::pass(detail) }
    }

    /// A pass when `ok`, else a fail, with `detail` either way.
    pub fn when(ok: bool, detail: impl Into<String>) -> Self {
        if ok { Self::pass(detail) } else { Self::fail(detail) }
    }

    /// The same verdict recording the values it compared.
    #[must_use]
    pub fn seen(mut self, before: Option<Value>, after: Option<Value>) -> Self {
        self.before = before;
        self.after = after;
        self
    }
}

/// One check's entry in the report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckResult {
    /// The check's id.
    pub id: String,
    /// Whether it decides the attempt.
    pub hard: bool,
    /// How it ended.
    pub outcome: Outcome,
    /// For a soft check that passed or failed: 1 on a pass, 0 on a fail (a number is not yet mapped through the check's
    /// `range`). Hard checks have no score, and neither has a check that errored: harness trouble is re-graded, never
    /// scored.
    pub score: Option<f64>,
    /// The value on the fixture, when the check looked at one.
    pub before: Option<Value>,
    /// The value on the result, when the check looked at one.
    pub after: Option<Value>,
    /// One line saying why.
    pub detail: String,
}

impl CheckResult {
    pub(super) fn new(id: &str, hard: bool, verdict: Verdict) -> Self {
        let score = match verdict.outcome {
            _ if hard => None,
            Outcome::Pass => Some(1.0),
            Outcome::Fail => Some(0.0),
            Outcome::Error => None,
        };
        Self {
            id: id.to_owned(),
            hard,
            outcome: verdict.outcome,
            score,
            before: verdict.before,
            after: verdict.after,
            detail: verdict.detail,
        }
    }
}

/// The grade of one attempt: one entry per check, in task order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckReport {
    /// The entries.
    pub results: Vec<CheckResult>,
}

impl CheckReport {
    /// Whether any check errored: the attempt must be re-graded, not scored.
    #[must_use]
    pub fn has_error(&self) -> bool {
        self.results.iter().any(|r| r.outcome == Outcome::Error)
    }

    /// Whether the attempt passed: every hard check passed, and no check (hard or soft) errored.
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.has_error() && self.results.iter().filter(|r| r.hard).all(|r| r.outcome == Outcome::Pass)
    }

    /// The report as JSON text, the same bytes for the same report.
    ///
    /// # Errors
    /// A serialisation error, which the report's types cannot produce; returned rather than hidden.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}
