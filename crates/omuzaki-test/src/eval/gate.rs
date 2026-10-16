//! The gate (design 8): reads the eval history and says which tasks and which suites regressed.
//!
//! It scores the **reference driver** only. The scripted driver is exact (every pass script passes, every fail script
//! fails, or the run itself fails), so a pass rate over scripted attempts says nothing the run did not.
//!
//! - A **series** is (task, task version, model, effort). A series with fewer than [`PROVISIONAL_UNDER`] scored attempts
//!   (`pass` and `fail`; refusals, errors and skips are not scored) is provisional: reported, never flagged.
//! - **Per task:** the window is the latest [`WINDOW`] scored attempts. The baseline rate `p0` is the pooled pass rate of
//!   the series' scored attempts in the [`BASELINE_DAYS`] days before the window began. The task regressed when
//!   `P(X <= k | n = 10, p0) < ` [`ALPHA`], with `k` the window's passes (`p0` held to at most [`P0_MAX`]).
//! - **Per suite** (per model): the night's rate is the mean of the per-task pass rates of the non-provisional tasks that
//!   ran that night. The suite regressed when it is below the median of the previous [`NIGHTS`] nights by more than
//!   `max(`[`FLOOR_POINTS`]` points, 3 x MAD of those nights)`.
//! - **Weak** (not a regression): a non-provisional task whose baseline is under [`WEAK_BELOW`].
//! - **Warnings**, never gating: cost per attempt up by more than [`COST_JUMP`] at about the same pass rate, and attempts
//!   with more than one turn that read nothing from the prompt cache.
//!
//! Every rate comes with a 95% Wilson interval.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use serde::Serialize;

use super::clock::run_started;
use super::history::{AttemptOutcome, AttemptRecord, RunRecord};

/// The driver whose attempts the gate scores.
pub const REFERENCE: &str = "reference";
/// Scored attempts a series needs before it can be flagged.
pub const PROVISIONAL_UNDER: usize = 20;
/// Scored attempts in a window.
pub const WINDOW: usize = 10;
/// Days of history before the window that make the baseline.
pub const BASELINE_DAYS: u64 = 28;
/// A task regressed when its window is less probable than this under its baseline.
pub const ALPHA: f64 = 0.01;
/// Nights a suite's trailing median is taken over.
pub const NIGHTS: usize = 14;
/// The least a suite may fall below its median before it regresses, as a fraction (5 points).
pub const FLOOR_POINTS: f64 = 0.05;
/// How many times the median absolute deviation widens the suite tolerance.
pub const MAD_WIDTH: f64 = 3.0;
/// A task whose baseline is below this is weak.
pub const WEAK_BELOW: f64 = 0.5;
/// A rise in cost per attempt, as a fraction, that earns a warning.
pub const COST_JUMP: f64 = 0.25;
/// Pass rates within this many points count as equal when cost is compared.
pub const EQUAL_RATE: f64 = 0.10;
/// The largest baseline rate the tail is taken under. A pooled rate of 100% is a measurement of the past, not a promise: with
/// `p0 = 1` a single miss in the window would be "impossible" and flag. Under 99%, one miss in ten (`P = 0.096`) is a
/// normal night, and two (`0.004`) are not.
pub const P0_MAX: f64 = 0.99;
/// The z value of a 95% interval.
const Z95: f64 = 1.96;
/// The most failing or passing transcripts a task's entry lists.
const TRANSCRIPTS: usize = 5;

/// A pass rate with its interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rate {
    /// Passes.
    pub passes: usize,
    /// Scored attempts.
    pub n: usize,
    /// `passes / n`.
    pub rate: f64,
    /// The 95% Wilson interval, low and high.
    pub wilson: [f64; 2],
}

impl Rate {
    /// The rate of `passes` out of `n` scored attempts.
    #[must_use]
    pub fn new(passes: usize, n: usize) -> Self {
        Self { passes, n, rate: ratio(passes, n), wilson: wilson(passes, n) }
    }
}

#[allow(clippy::cast_precision_loss)] // attempt counts are far below 2^52
fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 / whole as f64 }
}

/// The 95% Wilson score interval of `passes` out of `n`; `[0, 1]` when there is nothing to measure.
#[must_use]
#[allow(clippy::cast_precision_loss)] // counts of attempts are far below 2^52
pub fn wilson(passes: usize, n: usize) -> [f64; 2] {
    if n == 0 {
        return [0.0, 1.0];
    }
    let (n, p) = (n as f64, passes as f64 / n as f64);
    let z2 = Z95 * Z95;
    let centre = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let half = Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / (1.0 + z2 / n);
    [(centre - half).max(0.0), (centre + half).min(1.0)]
}

/// `total / count`, or 0 when there is nothing to average.
#[allow(clippy::cast_precision_loss)] // counts of attempts are far below 2^52
fn mean(total: f64, count: usize) -> f64 {
    if count == 0 { 0.0 } else { total / count as f64 }
}

/// `P(X <= k)` for `X` binomial with `n` trials and success chance `p`.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
pub fn binomial_cdf(n: usize, p: f64, k: usize) -> f64 {
    if k >= n {
        return 1.0;
    }
    let mut total = 0.0;
    // C(n, i) built up from C(n, 0), which stays exact in f64 for the small n of a window.
    let mut choose = 1.0_f64;
    for i in 0..=k {
        if i > 0 {
            choose = choose * (n - i + 1) as f64 / i as f64;
        }
        total += choose * p.powi(i as i32) * (1.0 - p).powi((n - i) as i32);
    }
    total.min(1.0)
}

/// Where a task's series stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Fewer than [`PROVISIONAL_UNDER`] scored attempts: reported, not gated.
    Provisional,
    /// Not enough history to judge: the window is short, or nothing precedes it within [`BASELINE_DAYS`].
    NoBaseline,
    /// The window is as the baseline says it should be.
    Ok,
    /// The window is improbably low under the baseline.
    Regression,
}

/// What the gate says about one series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskGate {
    /// The task.
    pub task: String,
    /// Its version.
    pub version: u32,
    /// The model.
    pub model: Option<String>,
    /// The effort.
    pub effort: Option<String>,
    /// Scored attempts in the whole series.
    pub scored: usize,
    /// The verdict.
    pub status: Status,
    /// Whether the baseline is under [`WEAK_BELOW`]: the tools, not the model, are to blame.
    pub weak: bool,
    /// The latest [`WINDOW`] scored attempts.
    pub window: Rate,
    /// The scored attempts of the [`BASELINE_DAYS`] days before the window, when there are any.
    pub baseline: Option<Rate>,
    /// `P(X <= k | n, p0)`, when the window and baseline allow it.
    pub probability: Option<f64>,
    /// The runs the window's attempts came from, oldest first.
    pub window_runs: Vec<String>,
    /// The commits of the window's attempts, oldest first: the changes the drop can come from.
    pub window_commits: Vec<String>,
    /// The commit of the latest passing baseline attempt, which is where the task last did well.
    pub last_good_commit: Option<String>,
    /// Transcript folders of the window's failing attempts, newest first.
    pub failing: Vec<String>,
    /// Transcript folders of the latest passing attempts, newest first: replayed as scripts at the new commit.
    pub passing: Vec<String>,
}

/// What the gate says about a model's suite on its latest night.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SuiteGate {
    /// The model.
    pub model: String,
    /// The night's run.
    pub run: String,
    /// The night's rate: the mean of the per-task pass rates of the non-provisional tasks that ran.
    pub rate: Option<f64>,
    /// The median of the previous nights, when there are [`NIGHTS`] of them.
    pub median: Option<f64>,
    /// The tolerance below the median: `max(5 points, 3 x MAD)`.
    pub tolerance: Option<f64>,
    /// `true` when the night is below `median - tolerance`.
    pub regression: bool,
}

/// Something worth saying that does not gate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning {
    /// The task.
    pub task: String,
    /// What.
    pub message: String,
}

/// The gate's answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateReport {
    /// Every series, by task, model and effort.
    pub tasks: Vec<TaskGate>,
    /// Each model's latest nightly suite.
    pub suites: Vec<SuiteGate>,
    /// Warnings.
    pub warnings: Vec<Warning>,
}

impl GateReport {
    /// Whether anything regressed, which fails the gate.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.tasks.iter().any(|t| t.status == Status::Regression) || self.suites.iter().any(|s| s.regression)
    }
}

type SeriesKey = (String, u32, Option<String>, Option<String>);

fn key(record: &AttemptRecord) -> SeriesKey {
    (record.task.clone(), record.task_version, record.model.clone(), record.effort.clone())
}

fn time_of(record: &AttemptRecord) -> Option<SystemTime> {
    run_started(&record.run)
}

fn passes(records: &[&AttemptRecord]) -> usize {
    records.iter().filter(|r| r.outcome == AttemptOutcome::Pass).count()
}

#[allow(clippy::cast_precision_loss)] // micro-dollars of a run are far below 2^52
fn mean_cost(records: &[&AttemptRecord]) -> f64 {
    mean(records.iter().map(|r| r.usd_micros).sum::<u64>() as f64, records.len())
}

/// Reads the history and runs the gate over it.
#[must_use]
pub fn gate(attempts: &[AttemptRecord], runs: &[RunRecord]) -> GateReport {
    // Only attempts of a run that finished and can be trusted count: a run whose record is missing was cut short, and one
    // that is not `valid` had too many errors for its rates to mean anything (design 7.2).
    let trusted: std::collections::BTreeSet<&str> = runs.iter().filter(|r| r.valid).map(|r| r.run.as_str()).collect();
    let reference: Vec<&AttemptRecord> =
        attempts.iter().filter(|a| a.driver == REFERENCE && trusted.contains(a.run.as_str())).collect();
    let mut series: BTreeMap<SeriesKey, Vec<&AttemptRecord>> = BTreeMap::new();
    for record in &reference {
        series.entry(key(record)).or_default().push(record);
    }
    // Runs that overlap append in the order they finish, not the order they began: the window is the latest by time.
    for records in series.values_mut() {
        records.sort_by(|a, b| (time_of(a), &a.run, a.attempt).cmp(&(time_of(b), &b.run, b.attempt)));
    }
    let mut tasks = Vec::new();
    let mut warnings = Vec::new();
    for (key, records) in &series {
        let scored: Vec<&AttemptRecord> = records.iter().copied().filter(|r| r.outcome.is_scored()).collect();
        tasks.push(task_gate(key, &scored));
        warnings.extend(cost_warning(key, &scored));
        warnings.extend(cache_warnings(key, records));
    }
    let non_provisional: Vec<&SeriesKey> = series
        .iter()
        .filter(|(_, r)| r.iter().filter(|a| a.outcome.is_scored()).count() >= PROVISIONAL_UNDER)
        .map(|(k, _)| k)
        .collect();
    let suites = suite_gates(&reference, runs, &non_provisional);
    GateReport { tasks, suites, warnings }
}

fn task_gate(key: &SeriesKey, scored: &[&AttemptRecord]) -> TaskGate {
    let (task, version, model, effort) = key.clone();
    let split = scored.len().saturating_sub(WINDOW);
    let (before_window, window) = scored.split_at(split);
    let window_rate = Rate::new(passes(window), window.len());
    let start = window.first().and_then(|r| time_of(r));
    let baseline_records: Vec<&AttemptRecord> = match start {
        Some(start) => {
            let from = start.checked_sub(Duration::from_secs(BASELINE_DAYS * 86_400));
            before_window
                .iter()
                .copied()
                .filter(|r| time_of(r).is_some_and(|t| t < start && from.is_none_or(|from| t >= from)))
                .collect()
        }
        None => Vec::new(),
    };
    let baseline = (!baseline_records.is_empty()).then(|| Rate::new(passes(&baseline_records), baseline_records.len()));
    let provisional = scored.len() < PROVISIONAL_UNDER;
    let probability = match (&baseline, window.len() == WINDOW) {
        (Some(base), true) => Some(binomial_cdf(WINDOW, base.rate.min(P0_MAX), window_rate.passes)),
        _ => None,
    };
    let status = if provisional {
        Status::Provisional
    } else if let Some(p) = probability {
        if p < ALPHA { Status::Regression } else { Status::Ok }
    } else {
        Status::NoBaseline
    };
    let newest_first = |outcome: AttemptOutcome, from: &[&AttemptRecord]| -> Vec<String> {
        from.iter().rev().filter(|r| r.outcome == outcome).take(TRANSCRIPTS).map(|r| r.transcript.clone()).collect()
    };
    let mut window_runs: Vec<String> = Vec::new();
    let mut window_commits: Vec<String> = Vec::new();
    for record in window {
        if !window_runs.contains(&record.run) {
            window_runs.push(record.run.clone());
        }
        if !window_commits.contains(&record.commit) {
            window_commits.push(record.commit.clone());
        }
    }
    TaskGate {
        task,
        version,
        model,
        effort,
        scored: scored.len(),
        status,
        weak: !provisional && baseline.is_some_and(|b| b.rate < WEAK_BELOW),
        window: window_rate,
        baseline,
        probability,
        window_runs,
        window_commits,
        last_good_commit: baseline_records
            .iter()
            .rev()
            .find(|r| r.outcome == AttemptOutcome::Pass)
            .map(|r| r.commit.clone()),
        failing: newest_first(AttemptOutcome::Fail, window),
        passing: newest_first(AttemptOutcome::Pass, scored),
    }
}

/// Cost per attempt up by more than [`COST_JUMP`] against the baseline, at about the same pass rate.
fn cost_warning(key: &SeriesKey, scored: &[&AttemptRecord]) -> Option<Warning> {
    let split = scored.len().saturating_sub(WINDOW);
    let (before, window) = scored.split_at(split);
    if before.is_empty() || window.len() < WINDOW {
        return None;
    }
    let (now, then) = (mean_cost(window), mean_cost(before));
    let equal = (ratio(passes(window), window.len()) - ratio(passes(before), before.len())).abs() <= EQUAL_RATE;
    (equal && then > 0.0 && now > then * (1.0 + COST_JUMP)).then(|| Warning {
        task: key.0.clone(),
        message: format!(
            "cost per attempt rose from {then:.0} to {now:.0} micro-dollars at about the same pass rate: the tool list or the results got heavier"
        ),
    })
}

/// Attempts that took more than one turn and read nothing from the prompt cache: something invalidates the prefix.
fn cache_warnings(key: &SeriesKey, records: &[&AttemptRecord]) -> Vec<Warning> {
    records
        .iter()
        .filter(|r| r.turns > 1 && r.usage.cache_read == 0)
        .map(|r| Warning {
            task: key.0.clone(),
            message: format!("{} read nothing from the prompt cache after turn 1 ({} turns)", r.transcript, r.turns),
        })
        .collect()
}

/// The median of `values` (the mean of the middle two when there is an even number).
#[must_use]
pub fn median(values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        n if n % 2 == 1 => sorted.get(middle).copied(),
        _ => Some(f64::midpoint(sorted.get(middle.checked_sub(1)?).copied()?, sorted.get(middle).copied()?)),
    }
}

/// Whether a night at `rate` is a regression against the nights before it, and the median and tolerance that say so.
/// `None` until there are [`NIGHTS`] previous nights to take the median of.
#[must_use]
pub fn suite_verdict(previous: &[f64], rate: f64) -> Option<(f64, f64, bool)> {
    let trailing = previous.get(previous.len().checked_sub(NIGHTS)?..)?;
    let middle = median(trailing)?;
    let deviations: Vec<f64> = trailing.iter().map(|v| (v - middle).abs()).collect();
    let tolerance = FLOOR_POINTS.max(MAD_WIDTH * median(&deviations)?);
    Some((middle, tolerance, rate < middle - tolerance))
}

fn suite_gates(reference: &[&AttemptRecord], runs: &[RunRecord], non_provisional: &[&SeriesKey]) -> Vec<SuiteGate> {
    let nightly = |model: &str| -> Vec<&RunRecord> {
        runs.iter()
            .filter(|r| r.valid && r.kind == "nightly" && r.driver == REFERENCE && r.model.as_deref() == Some(model))
            .collect()
    };
    let models: std::collections::BTreeSet<&str> = runs
        .iter()
        .filter(|r| r.valid && r.kind == "nightly" && r.driver == REFERENCE)
        .filter_map(|r| r.model.as_deref())
        .collect();
    let mut out = Vec::new();
    for model in models {
        let nights = nightly(model);
        let rates: Vec<Option<f64>> =
            nights.iter().map(|run| night_rate(reference, &run.run, model, non_provisional)).collect();
        let Some((latest, earlier)) = nights.split_last() else { continue };
        let rate = rates.last().copied().flatten();
        let previous: Vec<f64> = rates[..earlier.len()].iter().filter_map(|r| *r).collect();
        let verdict = rate.and_then(|rate| suite_verdict(&previous, rate));
        out.push(SuiteGate {
            model: model.to_owned(),
            run: latest.run.clone(),
            rate,
            median: verdict.map(|v| v.0),
            tolerance: verdict.map(|v| v.1),
            regression: verdict.is_some_and(|v| v.2),
        });
    }
    out
}

/// A night's suite rate: the mean of the per-task pass rates of the non-provisional tasks that ran in `run`.
fn night_rate(reference: &[&AttemptRecord], run: &str, model: &str, non_provisional: &[&SeriesKey]) -> Option<f64> {
    let mut per_task: BTreeMap<SeriesKey, (usize, usize)> = BTreeMap::new();
    for record in
        reference.iter().filter(|r| r.run == run && r.model.as_deref() == Some(model) && r.outcome.is_scored())
    {
        let k = key(record);
        if non_provisional.contains(&&k) {
            let entry = per_task.entry(k).or_default();
            entry.1 += 1;
            entry.0 += usize::from(record.outcome == AttemptOutcome::Pass);
        }
    }
    if per_task.is_empty() {
        return None;
    }
    Some(mean(per_task.values().map(|(p, n)| ratio(*p, *n)).sum(), per_task.len()))
}

#[cfg(test)]
mod tests;
