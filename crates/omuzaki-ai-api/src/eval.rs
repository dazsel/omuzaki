//! Evaluation: the numbers a model card reports, the floors a slot sets, and the hook every slot implements
//! ([`Slot::evaluate`](crate::Slot::evaluate)) for both the quick smoke test (before a user model is enabled, ADR 0016
//! rule 4) and the full suite (nightly, for shipped models).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{FetchError, FileSource, RunError, Session, SlotRef};

/// How much to evaluate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EvalLevel {
    /// Seconds, on a few pinned public-domain or synthetic items, with quality floors. Run in the app before a user
    /// model is enabled, and after a shipped model is installed or upgraded.
    Smoke,
    /// The whole suite, for model cards and gates such as QG-09. Run by `omuzaki-test`, never in the app.
    Full,
}

/// Which way a metric improves.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Better {
    /// Smaller is better (error rates, latency).
    Lower,
    /// Larger is better (recall, agreement).
    Higher,
}

/// Where a test set's items come from. Results on real and synthetic data are **never averaged together**: a model
/// trained on our renders can look perfect on renders, and the gates that decide defaults (QG-09) read real sets
/// only (spec ML-01).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TestSetKind {
    /// Rendered or generated items (synthetic faces, generated tones).
    Synthetic,
    /// Recorded from the world (real webcam footage, real illustrations, read speech).
    Real,
}

/// A named, versioned set of test items.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TestSet {
    /// Its id, with a version: `librivox-en-smoke-1`.
    pub id: String,
    /// Real or synthetic.
    pub kind: TestSetKind,
}

/// One measured number.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metric {
    /// What was measured: `wer`, `blendshape_mae`, `p95_ms`.
    pub name: String,
    /// The value.
    pub value: f64,
    /// Which way is better.
    pub better: Better,
    /// What it was measured on.
    pub set: TestSet,
}

/// A metric and the floor it must meet, if any.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Check {
    /// The measurement.
    pub metric: Metric,
    /// The worst acceptable value (a ceiling when lower is better). `None` only reports.
    pub floor: Option<f64>,
}

impl Check {
    /// True if the value meets the floor. A NaN never does.
    pub fn passes(&self) -> bool {
        let value = self.metric.value;
        match self.floor {
            _ if value.is_nan() => false,
            None => true,
            Some(floor) => match self.metric.better {
                Better::Lower => value <= floor,
                Better::Higher => value >= floor,
            },
        }
    }
}

/// The result of [`Slot::evaluate`](crate::Slot::evaluate). Stored with the model, shown on the Models page and in
/// `model.list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvalReport {
    /// Smoke or full.
    pub level: EvalLevel,
    /// Every metric with its floor.
    pub checks: Vec<Check>,
}

impl EvalReport {
    /// True if there is at least one check and every check passes: an evaluation that measured nothing proves
    /// nothing.
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(Check::passes)
    }

    /// The checks that failed, for the "why is it disabled" message.
    pub fn failures(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| !c.passes())
    }
}

/// What an evaluation may use besides the session under test. The registry provides it in the app; `omuzaki-test`
/// provides it nightly.
pub trait EvalEnv: Send + Sync {
    /// Verified test fixtures (clips, images, transcripts), pinned and fetched like model files. Fixtures are
    /// public-domain, CC0 or our own synthetic data; never private test material.
    fn files(&self) -> &dyn FileSource;

    /// A session of the slot's shipped default, for evaluations that need another model: text to speech is checked
    /// by transcribing its output with the shipped speech recogniser (ADR 0016 rule 4).
    ///
    /// # Errors
    /// [`EvalError::Needs`] when that model isn't available.
    fn shipped(&self, slot: &SlotRef) -> Result<Box<dyn Session>, EvalError>;
}

/// Why an evaluation could not produce a report. A report with failed checks is not an error: the model ran and
/// measured badly.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EvalError {
    /// A fixture could not be had.
    #[error("test data: {0}")]
    Fixture(#[from] FetchError),
    /// The model under test failed to run.
    #[error("the model failed while being tested: {0}")]
    Run(#[from] RunError),
    /// Another slot's model the evaluation needs isn't available.
    #[error("this test needs a working {0} model first")]
    Needs(SlotRef),
    /// The output could not be read (not UTF-8, wrong length).
    #[error("the model's output can't be read: {0}")]
    Output(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(value: f64, better: Better, floor: Option<f64>) -> Check {
        let set = TestSet { id: "s-1".into(), kind: TestSetKind::Synthetic };
        Check { metric: Metric { name: "m".into(), value, better, set }, floor }
    }

    #[test]
    fn floors_respect_direction_and_nan_fails() {
        assert!(check(0.2, Better::Lower, Some(0.3)).passes());
        assert!(!check(0.4, Better::Lower, Some(0.3)).passes());
        assert!(check(0.9, Better::Higher, Some(0.8)).passes());
        assert!(!check(f64::NAN, Better::Higher, None).passes());
        assert!(check(5.0, Better::Lower, None).passes(), "a report-only metric passes");
    }

    #[test]
    fn an_empty_report_does_not_pass() {
        let empty = EvalReport { level: EvalLevel::Smoke, checks: vec![] };
        assert!(!empty.passed());
        let one = EvalReport { level: EvalLevel::Smoke, checks: vec![check(0.4, Better::Lower, Some(0.3))] };
        assert!(!one.passed());
        assert_eq!(one.failures().count(), 1);
    }
}
