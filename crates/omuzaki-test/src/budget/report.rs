//! The budget report: measurements judged against the §10 table and against the previous run, as markdown and JSON.

use serde::{Deserialize, Serialize};

use super::error::BudgetError;
use super::stats::Summary;
use super::table::{Budget, Statistic, Unit};

/// Largest worsening against the previous run that still passes (§34: more than 10% fails).
pub const REGRESSION_LIMIT: f64 = 0.10;
/// Samples a metric without a budget needs before a change against the previous run counts as a regression: a single
/// start-up or memory reading moves more than 10% from run to run on a shared runner.
pub const MIN_REGRESSION_SAMPLES: u64 = 30;
/// Layout version of the JSON, bumped when a field changes meaning.
pub const SCHEMA: u32 = 1;
/// Every number this harness takes is `reported` (§10, "Numbers from other hardware"): it names the box it ran on and
/// never counts as a tier's number.
pub const CLASS: &str = "reported";

/// The box a report was taken on: counts only, no hardware names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// Logical CPUs available to the process.
    pub vcpus: usize,
    /// Operating system family (`std::env::consts::OS`).
    pub os: String,
    /// CPU architecture (`std::env::consts::ARCH`).
    pub arch: String,
}

impl Machine {
    /// The machine this process runs on. A failed CPU count is recorded as 0 rather than guessed.
    pub fn current() -> Self {
        Self {
            vcpus: std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }
    }
}

/// What was measured under one key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    /// The key: a §10 budget key to be judged, anything else is only reported.
    pub key: String,
    /// The unit of every statistic.
    pub unit: Unit,
    /// Samples behind the statistics.
    pub samples: u64,
    /// Samples refused for lack of buffer; non-zero means the run was truncated.
    pub dropped: u64,
    /// The statistics.
    pub stats: Summary,
}

impl Metric {
    /// A metric from timing samples in nanoseconds (reported in milliseconds); `None` without samples.
    #[allow(clippy::cast_precision_loss)] // nanosecond samples stay far below 2^52 (about 52 days)
    pub fn from_ns(key: &str, samples_ns: &[u64], dropped: u64) -> Option<Self> {
        let mut ms: Vec<f64> = samples_ns.iter().map(|&n| n as f64 / 1e6).collect();
        Some(Self {
            key: key.to_owned(),
            unit: Unit::Ms,
            samples: samples_ns.len() as u64,
            dropped,
            stats: Summary::of(&mut ms)?,
        })
    }

    /// A metric that is one number (peak memory, a start-up total).
    pub fn single(key: &str, unit: Unit, value: f64) -> Option<Self> {
        let stats = Summary::of(&mut [value])?;
        Some(Self { key: key.to_owned(), unit, samples: 1, dropped: 0, stats })
    }

    fn statistic(&self, which: Statistic) -> Option<f64> {
        match which {
            Statistic::P95 => Some(self.stats.p95),
            Statistic::Mean => Some(self.stats.mean),
            Statistic::Max => Some(self.stats.max),
            Statistic::Other => None,
        }
    }
}

/// The outcome of comparing a metric with its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// At or under the limit.
    Within,
    /// Over the limit.
    Over,
    /// The metric's unit differs from the budget's: a wiring mistake, never a pass.
    UnitMismatch,
    /// The key has a budget the harness cannot compare (two limits, or a statistic it does not compute).
    NotComparable,
    /// The key is not a budget row.
    NoBudget,
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// The measurement.
    pub metric: Metric,
    /// The budget text from the spec, when the key is a budget row.
    pub budget: Option<String>,
    /// The comparison with the budget.
    pub verdict: Verdict,
    /// The budget statistic's value for this run.
    pub value: Option<f64>,
    /// The same value in the previous report, when it was comparable.
    pub previous: Option<f64>,
    /// Change against the previous value in percent (positive is slower or larger).
    pub change_pct: Option<f64>,
    /// More than [`REGRESSION_LIMIT`] worse than the previous value.
    pub regression: bool,
}

/// A whole run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// [`SCHEMA`].
    pub schema: u32,
    /// [`CLASS`].
    pub class: String,
    /// The box it ran on.
    pub machine: Machine,
    /// One row per metric, ordered by key.
    pub rows: Vec<Row>,
    /// Things a reader needs to judge the numbers (a missing counter, an incomparable previous run).
    pub notes: Vec<String>,
}

impl Report {
    /// Judge `metrics` against `budgets` and, when given, the `previous` report. A previous report from a machine with
    /// a different CPU count is ignored with a note: its numbers say nothing about a regression.
    pub fn build(
        machine: Machine,
        mut metrics: Vec<Metric>,
        budgets: &[Budget],
        previous: Option<&Report>,
        mut notes: Vec<String>,
    ) -> Self {
        metrics.sort_by(|a, b| a.key.cmp(&b.key));
        let previous = previous.filter(|p| {
            let comparable = p.machine == machine;
            if !comparable {
                notes.push("previous report is from a different machine; no regression check".to_owned());
            }
            comparable
        });
        let rows = metrics.into_iter().map(|m| row(m, budgets, previous)).collect();
        Self { schema: SCHEMA, class: CLASS.to_owned(), machine, rows, notes }
    }

    /// Rows over budget, with a unit mismatch, or more than [`REGRESSION_LIMIT`] worse than the previous run.
    pub fn violations(&self) -> Vec<String> {
        let mut out = Vec::new();
        for r in &self.rows {
            let key = &r.metric.key;
            match r.verdict {
                Verdict::Over => out.push(format!("{key}: over budget")),
                Verdict::UnitMismatch => out.push(format!("{key}: unit differs from the budget")),
                _ => {}
            }
            if r.metric.dropped > 0 {
                out.push(format!("{key}: {} samples dropped", r.metric.dropped));
            }
            if r.regression {
                out.push(format!("{key}: regression of {:.1}%", r.change_pct.unwrap_or(f64::NAN)));
            }
        }
        out
    }

    /// The report as pretty JSON.
    ///
    /// # Errors
    /// [`BudgetError::Json`] if serialisation fails.
    pub fn to_json(&self) -> Result<String, BudgetError> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    /// Read a report back from its JSON.
    ///
    /// # Errors
    /// [`BudgetError::Json`] on malformed input.
    pub fn from_json(text: &str) -> Result<Self, BudgetError> {
        Ok(serde_json::from_str(text)?)
    }

    /// The report as markdown.
    pub fn to_markdown(&self) -> String {
        let m = &self.machine;
        let mut lines = vec![
            "# Budget report".to_owned(),
            String::new(),
            format!(
                "Class `{}`: measured on {} {} with {} vCPUs; never a tier's number.",
                self.class, m.os, m.arch, m.vcpus
            ),
            String::new(),
            "| Key | Samples | p50 | p95 | p99 | max | Budget | Verdict | Change |".to_owned(),
            "|---|---|---|---|---|---|---|---|---|".to_owned(),
        ];
        for r in &self.rows {
            let (u, st) = (r.metric.unit, &r.metric.stats);
            let change = r
                .change_pct
                .map_or("-".to_owned(), |c| format!("{c:+.1}%{}", if r.regression { " **regression**" } else { "" }));
            lines.push(format!(
                "| `{}` | {} | {} | {} | {} | {} | {} | {:?} | {change} |",
                r.metric.key,
                r.metric.samples,
                fmt(st.p50, u),
                fmt(st.p95, u),
                fmt(st.p99, u),
                fmt(st.max, u),
                r.budget.as_deref().unwrap_or("-"),
                r.verdict,
            ));
        }
        if !self.notes.is_empty() {
            lines.push(String::new());
            lines.extend(self.notes.iter().map(|n| format!("- {n}")));
        }
        lines.join("\n") + "\n"
    }
}

fn row(metric: Metric, budgets: &[Budget], previous: Option<&Report>) -> Row {
    let budget = budgets.iter().find(|b| b.key == metric.key);
    let value = budget.and_then(|b| metric.statistic(b.statistic));
    let verdict = match (budget, budget.and_then(|b| b.limit), value) {
        (None, ..) => Verdict::NoBudget,
        (Some(_), Some(l), _) if l.unit != metric.unit => Verdict::UnitMismatch,
        (Some(_), Some(l), Some(v)) if v <= l.value => Verdict::Within,
        (Some(_), Some(_), Some(_)) => Verdict::Over,
        _ => Verdict::NotComparable,
    };
    // Keys without a budget are still compared with the previous run, on the p95.
    let stat = value.unwrap_or(metric.stats.p95);
    let prev = previous
        .and_then(|p| p.rows.iter().find(|r| r.metric.key == metric.key && r.metric.unit == metric.unit))
        .and_then(|r| r.value.or(Some(r.metric.stats.p95)))
        .filter(|&p| p > 0.0);
    let change_pct = prev.map(|p| (stat - p) / p * 100.0);
    let trusted = budget.is_some() || metric.samples >= MIN_REGRESSION_SAMPLES;
    let regression = trusted && change_pct.is_some_and(|c| c > REGRESSION_LIMIT * 100.0);
    Row {
        budget: budget.map(|b| b.budget_text.clone()),
        verdict,
        value,
        previous: prev,
        change_pct,
        regression,
        metric,
    }
}

fn fmt(v: f64, unit: Unit) -> String {
    match unit {
        Unit::Ms => format!("{v:.3} ms"),
        Unit::Percent => format!("{v:.2} %"),
        Unit::Bytes => format!("{:.1} MiB", v / (1024.0 * 1024.0)),
        Unit::Count => format!("{v}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::table::parse_spec;

    const SPEC: &str = "## 10. P\n| Key | Item | Budget | Tier | Statistic | Gate |\n|-|-|-|-|-|-|\n\
| `physics.step` | p | ≤ 0.5 ms | `R` | p95 | g |\n| `ram.runtime` | r | ≤ 400 MB | `R` | max over 5 min | g |\n\
| `asr.live` | a | CPU ≤ 1.5 %, GPU ≤ 1 ms | `R` | mean | g |\n";

    fn machine(vcpus: usize) -> Machine {
        Machine { vcpus, os: "linux".into(), arch: "x86_64".into() }
    }

    fn us(key: &str, micros: u64) -> Metric {
        Metric::from_ns(key, &[micros * 1000; 100], 0).unwrap()
    }

    fn report(m: Vec<Metric>, prev: Option<&Report>) -> Report {
        Report::build(machine(4), m, &parse_spec(SPEC).unwrap(), prev, vec![])
    }

    #[test]
    fn judges_against_the_budget() {
        let ram = Metric::single("ram.runtime", Unit::Bytes, 500.0 * 1024.0 * 1024.0).unwrap();
        let r =
            report(vec![us("physics.step", 400), ram, us("asr.live", 9000), us("other", 1000), us("zzz", 1000)], None);
        let v: Vec<_> = r.rows.iter().map(|r| (r.metric.key.as_str(), r.verdict)).collect();
        assert_eq!(
            v,
            [
                ("asr.live", Verdict::NotComparable),
                ("other", Verdict::NoBudget),
                ("physics.step", Verdict::Within),
                ("ram.runtime", Verdict::Over),
                ("zzz", Verdict::NoBudget)
            ]
        );
        assert_eq!(r.violations(), ["ram.runtime: over budget"]);
        let wrong = Metric::single("physics.step", Unit::Percent, 1.0).unwrap();
        assert_eq!(report(vec![wrong], None).rows[0].verdict, Verdict::UnitMismatch);
    }

    #[test]
    fn an_injected_slowdown_trips_the_regression_check() {
        let before = report(vec![us("physics.step", 400)], None);
        assert_eq!(report(vec![us("physics.step", 430)], Some(&before)).violations(), Vec::<String>::new());
        let slow = report(vec![us("physics.step", 450)], Some(&before));
        assert!(slow.rows[0].regression);
        assert_eq!(slow.violations(), ["physics.step: regression of 12.5%"]);
    }

    #[test]
    fn a_noisy_single_reading_without_a_budget_is_not_a_regression() {
        let one = |v| Metric::single("harness.startup", Unit::Ms, v).unwrap();
        let before = report(vec![one(1.0)], None);
        let r = report(vec![one(2.0)], Some(&before));
        assert!(!r.rows[0].regression && r.rows[0].change_pct.is_some());
    }

    #[test]
    fn a_previous_run_from_another_machine_is_not_compared() {
        let mut before = report(vec![us("physics.step", 100)], None);
        before.machine.vcpus = 64;
        let r = report(vec![us("physics.step", 400)], Some(&before));
        assert!(!r.rows[0].regression && r.rows[0].previous.is_none());
        assert_eq!(r.notes.len(), 1);
    }

    #[test]
    fn truncated_runs_are_violations_and_json_round_trips() {
        let mut m = us("physics.step", 100);
        m.dropped = 3;
        let r = report(vec![m], None);
        assert_eq!(r.violations(), ["physics.step: 3 samples dropped"]);
        assert_eq!(Report::from_json(&r.to_json().unwrap()).unwrap(), r);
        let md = r.to_markdown();
        assert!(md.contains("| `physics.step` | 100 | 0.100 ms") && md.contains("4 vCPUs"));
    }
}
