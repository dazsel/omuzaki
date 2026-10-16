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
pub const SCHEMA: u32 = 2;

/// What a report's numbers are, in the sense of spec §10 ("Numbers from other hardware"). The class decides what may
/// fail a gating run; the caller states it because only the caller knows what machine it ran on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Taken on any other machine (a cloud runner, a laptop): printed with the machine, never gates.
    Reported,
    /// Taken on the tier it is claimed for: judged against the budget directly. The runner needs the
    /// [`Provenance`] spec §10 requires (tier, OS build, GPU adapter, backend).
    Measured,
    /// Taken on another machine and converted by a spec rule: judged against the budget directly. Needs the same
    /// [`Provenance`] plus the scaling rule.
    Scaled,
    /// A PR against main on the same machine, alternating runs in one session: a budgeted metric more than 10% worse
    /// fails. A strict run needs the session named in both reports ([`Provenance::session`]).
    Relative,
}

impl Class {
    /// The class named by `name` (`reported`, `measured`, `scaled` or `relative`).
    ///
    /// # Errors
    /// [`BudgetError::Class`] for any other name.
    pub fn parse(name: &str) -> Result<Self, BudgetError> {
        match name {
            "reported" => Ok(Self::Reported),
            "measured" => Ok(Self::Measured),
            "scaled" => Ok(Self::Scaled),
            "relative" => Ok(Self::Relative),
            other => Err(BudgetError::Class(other.to_owned())),
        }
    }
}

impl Class {
    /// What a reader may conclude from numbers of this class, for the report's heading.
    fn meaning(self) -> &'static str {
        match self {
            Self::Reported => "never a tier's number",
            Self::Measured => "taken on the tier it is claimed for, judged against the budget",
            Self::Scaled => "converted from another machine by a spec rule, judged against the budget",
            Self::Relative => "judged only against the previous run on the same machine, not against the budget",
        }
    }
}

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

/// What spec §10 ("Numbers from other hardware") asks a gating number to state, given by the caller because only the
/// caller knows it. Names the kind of hardware, not a machine: no device ids.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The tier the numbers are claimed for.
    pub tier: String,
    /// The operating system build.
    pub os_build: String,
    /// The GPU adapter.
    pub gpu_adapter: String,
    /// The graphics driver or backend.
    pub backend: String,
    /// For `scaled` numbers, the spec rule that converted them.
    pub scaling_rule: Option<String>,
    /// An id shared by the alternating runs of one session, which a `relative` comparison needs from both reports.
    pub session: Option<String>,
}

impl Provenance {
    /// Read `OMUZAKI_BUDGET_TIER`, `_OS_BUILD`, `_GPU`, `_BACKEND`, `_SCALING_RULE` and `_SESSION`; `None` when none is
    /// set. An empty variable counts as not set.
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(format!("OMUZAKI_BUDGET_{k}")).ok().filter(|v| !v.trim().is_empty());
        let found = Self {
            tier: var("TIER").unwrap_or_default(),
            os_build: var("OS_BUILD").unwrap_or_default(),
            gpu_adapter: var("GPU").unwrap_or_default(),
            backend: var("BACKEND").unwrap_or_default(),
            scaling_rule: var("SCALING_RULE"),
            session: var("SESSION"),
        };
        (found != Self::default()).then_some(found)
    }

    /// Whether this provenance (or none) is enough for a gating `class`.
    ///
    /// # Errors
    /// [`BudgetError::Provenance`] naming the variables that are missing, for `measured` and `scaled`.
    pub fn require(provenance: Option<&Self>, class: Class) -> Result<(), BudgetError> {
        let (needs_rule, name) = match class {
            Class::Measured => (false, "measured"),
            Class::Scaled => (true, "scaled"),
            Class::Reported | Class::Relative => return Ok(()),
        };
        let none = Self::default();
        let p = provenance.unwrap_or(&none);
        let mut missing = Vec::new();
        for (value, var) in
            [(&p.tier, "TIER"), (&p.os_build, "OS_BUILD"), (&p.gpu_adapter, "GPU"), (&p.backend, "BACKEND")]
        {
            if value.trim().is_empty() {
                missing.push(format!("OMUZAKI_BUDGET_{var}"));
            }
        }
        if needs_rule && p.scaling_rule.is_none() {
            missing.push("OMUZAKI_BUDGET_SCALING_RULE".to_owned());
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(BudgetError::Provenance { class: name.to_owned(), missing: missing.join(", ") })
        }
    }
}

/// The role of a number in a `scaled` report (spec §10: for GPU rows the central estimate is a hard gate and a
/// pessimistic estimate over the budget only a warning). Every other number is a central one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Estimate {
    /// The measurement, or a scaled central estimate: judged against the budget.
    #[default]
    Central,
    /// A pessimistic scaled estimate: over the budget is a warning, never a failure.
    Pessimistic,
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
    /// The role of the number; a key may be reported once per role.
    #[serde(default)]
    pub estimate: Estimate,
}

impl Metric {
    /// This metric as a pessimistic estimate of its key.
    #[must_use]
    pub fn pessimistic(mut self) -> Self {
        self.estimate = Estimate::Pessimistic;
        self
    }

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
            estimate: Estimate::Central,
        })
    }

    /// A metric that is one number (peak memory, a start-up total).
    pub fn single(key: &str, unit: Unit, value: f64) -> Option<Self> {
        let stats = Summary::of(&mut [value])?;
        Some(Self { key: key.to_owned(), unit, samples: 1, dropped: 0, stats, estimate: Estimate::Central })
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
    /// A pessimistic estimate over the limit: a warning, never a failure.
    Warn,
    /// The budget row applies to other tiers than the one the report claims, so it is not judged.
    OtherTier,
}

/// Which findings a call wants.
#[derive(Clone, Copy)]
struct Scope {
    /// Rows over their budget.
    budget: bool,
    /// Rows worse than the previous run.
    regressions: bool,
    /// Count a regression only on a row that has a budget.
    budgeted_only: bool,
    /// Rows over budget on a pessimistic estimate.
    warnings: bool,
}

/// Whether a budget row's tier cell (`R`, `R`, `U`, `any`, `as R`) covers the `claimed` tier.
fn applies_to_tier(cell: &str, claimed: &str) -> bool {
    cell.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| word == "any" || word == claimed)
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// The measurement.
    pub metric: Metric,
    /// The budget text from the spec, when the key is a budget row.
    pub budget: Option<String>,
    /// The tier cell from the spec, when the key is a budget row.
    pub tier: Option<String>,
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
    /// What the numbers are; only a class other than [`Class::Reported`] can fail a gating run.
    pub class: Class,
    /// The box it ran on.
    pub machine: Machine,
    /// What the caller stated about the hardware and the session, if anything.
    pub provenance: Option<Provenance>,
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
        class: Class,
        mut notes: Vec<String>,
    ) -> Self {
        metrics.sort_by(|a, b| (&a.key, a.estimate).cmp(&(&b.key, b.estimate)));
        let previous = previous.filter(|p| {
            let comparable = p.machine == machine;
            if !comparable {
                notes.push("previous report is from a different machine; no regression check".to_owned());
            }
            comparable
        });
        let rows = metrics.into_iter().map(|m| row(m, budgets, previous)).collect();
        Self { schema: SCHEMA, class, machine, provenance: None, rows, notes }
    }

    /// This report with the provenance the caller stated.
    #[must_use]
    pub fn with_provenance(mut self, provenance: Option<Provenance>) -> Self {
        // A gating class judges a budget row only on a tier the row applies to (`latency.desktop` is for `D`).
        if let (Class::Measured | Class::Scaled, Some(claimed)) =
            (self.class, provenance.as_ref().map(|p| p.tier.trim()))
            && !claimed.is_empty()
        {
            for row in &mut self.rows {
                let applies = row.tier.as_deref().is_none_or(|cell| applies_to_tier(cell, claimed));
                if !applies && matches!(row.verdict, Verdict::Within | Verdict::Over | Verdict::Warn) {
                    row.verdict = Verdict::OtherTier;
                }
            }
        }
        self.provenance = provenance;
        self
    }

    /// How many rows were judged against a §10 budget (within it, over it, or over on a pessimistic estimate).
    pub fn judged(&self) -> usize {
        self.rows.iter().filter(|r| matches!(r.verdict, Verdict::Within | Verdict::Over | Verdict::Warn)).count()
    }

    /// How many rows with a §10 budget have a value from the previous report: what a `relative` run can fail on.
    pub fn compared_budgeted(&self) -> usize {
        self.rows.iter().filter(|r| r.budget.is_some() && r.previous.is_some()).count()
    }

    /// Every finding: rows over budget, with a unit mismatch, truncated, or more than [`REGRESSION_LIMIT`] worse than the
    /// previous run. Whether a finding fails a run is [`Report::failures`].
    pub fn violations(&self) -> Vec<String> {
        self.findings(Scope { budget: true, regressions: true, budgeted_only: false, warnings: true })
    }

    /// The findings that fail a gating run of this report's class. `reported` numbers never gate; `measured` and
    /// `scaled` ones are judged against the budget; `relative` ones against the previous run. A truncated run or a unit
    /// mismatch is a harness fault and fails every class but `reported`. A `relative` run fails only on a budgeted
    /// metric (spec §10); a change in any other metric is printed and not gated.
    pub fn failures(&self) -> Vec<String> {
        match self.class {
            Class::Reported => Vec::new(),
            Class::Measured | Class::Scaled => {
                self.findings(Scope { budget: true, regressions: false, budgeted_only: false, warnings: false })
            }
            Class::Relative => {
                self.findings(Scope { budget: false, regressions: true, budgeted_only: true, warnings: false })
            }
        }
    }

    fn findings(&self, scope: Scope) -> Vec<String> {
        let Scope { budget, regressions, budgeted_only, warnings } = scope;
        let mut out = Vec::new();
        for r in &self.rows {
            let key = &r.metric.key;
            match r.verdict {
                Verdict::Over if budget => out.push(format!("{key}: over budget")),
                Verdict::Warn if warnings => {
                    out.push(format!("{key}: over budget on the pessimistic estimate (warning)"))
                }
                Verdict::UnitMismatch => out.push(format!("{key}: unit differs from the budget")),
                _ => {}
            }
            if r.metric.dropped > 0 {
                out.push(format!("{key}: {} samples dropped", r.metric.dropped));
            }
            if regressions && r.regression && (!budgeted_only || r.budget.is_some()) {
                match r.change_pct {
                    Some(change) => out.push(format!("{key}: regression of {change:.1}%")),
                    None => out.push(format!("{key}: regression from zero")),
                }
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
    /// [`BudgetError::Json`] on malformed input; [`BudgetError::Schema`] when the file's `schema` is not [`SCHEMA`], so
    /// a layout whose fields mean something else is never compared as if it were this one.
    pub fn from_json(text: &str) -> Result<Self, BudgetError> {
        let report: Self = serde_json::from_str(text)?;
        if report.schema != SCHEMA {
            return Err(BudgetError::Schema { found: report.schema, supported: SCHEMA });
        }
        Ok(report)
    }

    /// The report as markdown.
    pub fn to_markdown(&self) -> String {
        let m = &self.machine;
        let mut lines = vec![
            "# Budget report".to_owned(),
            String::new(),
            format!(
                "Class `{}`: measured on {} {} with {} vCPUs; {}.",
                format!("{:?}", self.class).to_lowercase(),
                m.os,
                m.arch,
                m.vcpus,
                self.class.meaning()
            ),
            String::new(),
            "| Key | Samples | p50 | p95 | p99 | max | Budget | Verdict | Change |".to_owned(),
            "|---|---|---|---|---|---|---|---|---|".to_owned(),
        ];
        if let Some(p) = &self.provenance {
            let optional =
                |label: &str, v: &Option<String>| v.as_ref().map(|v| format!(", {label} {v}")).unwrap_or_default();
            lines.insert(3, String::new());
            lines.insert(
                4,
                format!(
                    "Provenance: tier {}, OS build {}, GPU {}, backend {}{}{}.",
                    p.tier,
                    p.os_build,
                    p.gpu_adapter,
                    p.backend,
                    optional("scaling rule", &p.scaling_rule),
                    optional("session", &p.session)
                ),
            );
        }
        for r in &self.rows {
            let (u, st) = (r.metric.unit, &r.metric.stats);
            let change = r
                .change_pct
                .map_or(if r.regression { "from zero **regression**".to_owned() } else { "-".to_owned() }, |c| {
                    format!("{c:+.1}%{}", if r.regression { " **regression**" } else { "" })
                });
            lines.push(format!(
                "| `{}`{} | {} | {} | {} | {} | {} | {} | {:?} | {change} |",
                r.metric.key,
                if r.metric.estimate == Estimate::Pessimistic { " (pessimistic)" } else { "" },
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
        (Some(_), Some(_), Some(_)) if metric.estimate == Estimate::Pessimistic => Verdict::Warn,
        (Some(_), Some(_), Some(_)) => Verdict::Over,
        _ => Verdict::NotComparable,
    };
    // Keys without a budget are still compared with the previous run, on the p95.
    let stat = value.unwrap_or(metric.stats.p95);
    let prev = previous
        .and_then(|p| {
            p.rows.iter().find(|r| {
                r.metric.key == metric.key && r.metric.unit == metric.unit && r.metric.estimate == metric.estimate
            })
        })
        .and_then(|r| r.value.or(Some(r.metric.stats.p95)))
        .filter(|&p| p >= 0.0);
    // A zero baseline (`floor.dropped_frames` is `≤ 0`) has no percentage; anything above it is an unbounded worsening.
    let change_pct = prev.filter(|&p| p > 0.0).map(|p| (stat - p) / p * 100.0);
    let from_zero = prev == Some(0.0) && stat > 0.0;
    let trusted = budget.is_some() || metric.samples >= MIN_REGRESSION_SAMPLES;
    let regression = trusted && (from_zero || change_pct.is_some_and(|c| c > REGRESSION_LIMIT * 100.0));
    Row {
        budget: budget.map(|b| b.budget_text.clone()),
        tier: budget.map(|b| b.tier.clone()),
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
        Report::build(machine(4), m, &parse_spec(SPEC).unwrap(), prev, Class::Relative, vec![])
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
    fn only_a_gating_class_fails_and_each_judges_what_its_class_may() {
        let before = report(vec![us("physics.step", 400)], None);
        let slow_and_over = report(vec![us("physics.step", 900)], Some(&before));
        let with = |class| Report { class, ..slow_and_over.clone() };
        assert_eq!(slow_and_over.violations().len(), 2);
        assert_eq!(with(Class::Reported).failures(), Vec::<String>::new());
        assert_eq!(with(Class::Measured).failures(), ["physics.step: over budget"]);
        assert_eq!(with(Class::Scaled).failures(), ["physics.step: over budget"]);
        assert_eq!(with(Class::Relative).failures(), ["physics.step: regression of 125.0%"]);
        assert!(Class::parse("relative").is_ok() && Class::parse("fast").is_err());
    }

    #[test]
    fn an_unsupported_schema_is_a_typed_error() {
        let mut r = report(vec![us("physics.step", 400)], None);
        r.schema = SCHEMA + 1;
        let err = Report::from_json(&r.to_json().unwrap()).unwrap_err();
        assert!(matches!(err, BudgetError::Schema { found, supported } if found == SCHEMA + 1 && supported == SCHEMA));
    }

    #[test]
    fn a_relative_run_gates_only_budgeted_metrics() {
        // `other` has no budget and 100 samples: it is 10x slower than last time, which is reported, not gated.
        let previous = report(vec![us("other", 1000)], None);
        let judged = report(vec![us("other", 10_000)], Some(&previous));
        assert!(judged.rows[0].regression, "the change is still reported");
        assert_eq!(judged.failures(), Vec::<String>::new());
        assert_eq!(judged.violations(), ["other: regression of 900.0%"]);
        assert_eq!(judged.compared_budgeted(), 0, "so a strict relative run has nothing to judge");
    }

    #[test]
    fn a_worsening_from_a_zero_baseline_is_a_regression() {
        let before = report(vec![us("physics.step", 0)], None);
        assert_eq!(before.rows[0].previous, Some(0.0));
        let worse = report(vec![us("physics.step", 400)], Some(&before));
        assert!(worse.rows[0].regression && worse.rows[0].change_pct.is_none());
        assert_eq!(worse.failures(), ["physics.step: regression from zero"]);
        assert_eq!(worse.compared_budgeted(), 1);
        assert!(worse.to_markdown().contains("from zero **regression**"));
        let still_zero = report(vec![us("physics.step", 0)], Some(&before));
        assert!(!still_zero.rows[0].regression);
        // And the JSON keeps it (no infinity in the file).
        assert_eq!(Report::from_json(&worse.to_json().unwrap()).unwrap(), worse);
    }

    #[test]
    fn a_budget_row_is_judged_only_on_a_tier_it_applies_to() {
        let tiered = |tier: &str| {
            let provenance = Provenance { tier: tier.into(), ..Provenance::default() };
            Report::build(
                machine(4),
                vec![us("physics.step", 900)],
                &parse_spec(SPEC).unwrap(),
                None,
                Class::Measured,
                vec![],
            )
            .with_provenance(Some(provenance))
        };
        // `physics.step` is a tier `R` row.
        assert_eq!(tiered("R").failures(), ["physics.step: over budget"]);
        let other = tiered("U");
        assert_eq!(other.rows[0].verdict, Verdict::OtherTier);
        assert_eq!((other.failures(), other.judged()), (Vec::<String>::new(), 0));
        assert!(applies_to_tier("`R`, `U`", "U") && applies_to_tier("any", "F") && applies_to_tier("as `R`", "R"));
        assert!(!applies_to_tier("`R`, `D`", "U"));
    }

    #[test]
    fn a_pessimistic_scaled_estimate_over_budget_is_a_warning_and_the_central_one_a_failure() {
        let ram = |mb: f64| Metric::single("ram.runtime", Unit::Bytes, mb * 1024.0 * 1024.0).unwrap();
        let scaled = |central: f64, pessimistic: f64| {
            let metrics = vec![ram(pessimistic).pessimistic(), ram(central)];
            Report::build(machine(4), metrics, &parse_spec(SPEC).unwrap(), None, Class::Scaled, vec![])
        };
        let warned = scaled(300.0, 500.0);
        let roles: Vec<_> = warned.rows.iter().map(|r| (r.metric.estimate, r.verdict)).collect();
        assert_eq!(roles, [(Estimate::Central, Verdict::Within), (Estimate::Pessimistic, Verdict::Warn)]);
        assert_eq!(warned.failures(), Vec::<String>::new());
        assert_eq!(warned.violations(), ["ram.runtime: over budget on the pessimistic estimate (warning)"]);
        assert!(warned.to_markdown().contains("`ram.runtime` (pessimistic)"));
        assert_eq!(scaled(500.0, 500.0).failures(), ["ram.runtime: over budget"]);
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
