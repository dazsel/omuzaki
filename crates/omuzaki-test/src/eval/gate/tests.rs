//! The gate's statistics on synthetic histories.

use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::eval::clock::utc_minute;
use crate::eval::history::{Counts, Oracles, SCHEMA, Spend, Usage};

/// The first day of the synthetic histories: 2026-09-01.
const DAY_ONE: u64 = 1_788_220_800;

fn run_id(day: u64, minute: u64) -> String {
    format!("{}-nightly", utc_minute(UNIX_EPOCH + Duration::from_secs(DAY_ONE + day * 86_400 + minute * 60)))
}

fn attempt(run: &str, task: &str, outcome: AttemptOutcome) -> AttemptRecord {
    AttemptRecord {
        v: SCHEMA,
        run: run.to_owned(),
        commit: format!("c-{}", &run[5..10]),
        family: "task".to_owned(),
        driver: REFERENCE.to_owned(),
        model: Some("m".to_owned()),
        effort: None,
        task: task.to_owned(),
        task_version: 1,
        task_hash: "b3:x".to_owned(),
        feature: "QG-03".to_owned(),
        attempt: 1,
        outcome,
        reason: None,
        checks: Vec::new(),
        turns: 3,
        tool_calls: 5,
        tool_errors: 0,
        retries: 0,
        usage: Usage { cache_read: 100, ..Usage::default() },
        usd_micros: 1000,
        wall_ms: 1,
        tools_hash: "b3:t".to_owned(),
        prompt_hash: "b3:p".to_owned(),
        harness: "0".to_owned(),
        transcript: format!("evals/runs/{run}/{task}/1/"),
        script: None,
    }
}

/// `n` attempts of `task` in the run of `day`, `passes` of them passing.
fn night(day: u64, task: &str, passes: usize, n: usize) -> Vec<AttemptRecord> {
    let run = run_id(day, 0);
    (0..n).map(|i| attempt(&run, task, if i < passes { AttemptOutcome::Pass } else { AttemptOutcome::Fail })).collect()
}

fn run_record(day: u64, model: &str) -> RunRecord {
    RunRecord {
        v: SCHEMA,
        run: run_id(day, 0),
        kind: "nightly".to_owned(),
        driver: REFERENCE.to_owned(),
        model: Some(model.to_owned()),
        commit: "c".to_owned(),
        tasks_run: Vec::new(),
        tasks_skipped: Vec::new(),
        attempts: Counts::default(),
        oracles: Oracles::default(),
        spend: Spend::default(),
        wall_ms: 0,
        valid: true,
    }
}

/// Thirty baseline attempts with 27 passes over days 20 to 29, then a window of ten on day 30 with `window_passes`.
fn history_at_ninety_percent(window_passes: usize) -> Vec<AttemptRecord> {
    let mut all = Vec::new();
    for day in 20..30 {
        all.extend(night(day, "t", 3 - usize::from(day < 23), 3));
    }
    all.extend(night(30, "t", window_passes, 10));
    all
}

/// Runs the gate with a valid run record for every run of `all` that `runs` does not already name.
fn gate_of(all: &[AttemptRecord], runs: &[RunRecord]) -> GateReport {
    let mut every = runs.to_vec();
    for id in all.iter().map(|a| a.run.as_str()).collect::<std::collections::BTreeSet<_>>() {
        if !every.iter().any(|r| r.run == id) {
            every.push(RunRecord { run: id.to_owned(), ..run_record(0, "m") });
        }
    }
    gate(all, &every)
}

fn only(report: &GateReport) -> &TaskGate {
    assert_eq!(report.tasks.len(), 1);
    &report.tasks[0]
}

#[test]
fn the_binomial_tail_matches_the_values_the_design_names() {
    assert!((binomial_cdf(10, 0.9, 5) - 0.001_635).abs() < 1e-5);
    assert!((binomial_cdf(10, 0.9, 6) - 0.012_795).abs() < 1e-5);
    assert!((binomial_cdf(10, 0.6, 1) - 0.001_678).abs() < 1e-5);
    assert!((binomial_cdf(10, 0.6, 2) - 0.012_295).abs() < 1e-5);
    assert_eq!(binomial_cdf(10, 0.5, 10), 1.0);
    assert!((binomial_cdf(10, 0.0, 0) - 1.0).abs() < 1e-12);
    assert!(binomial_cdf(10, 1.0, 9) < 1e-12);
}

#[test]
fn a_wilson_interval_widens_for_few_attempts_and_covers_the_rate() {
    let [low, high] = wilson(5, 10);
    assert!((low - 0.237).abs() < 0.002 && (high - 0.763).abs() < 0.002, "{low} {high}");
    let [low, high] = wilson(50, 100);
    assert!(high - low < 0.2);
    assert_eq!(wilson(0, 0), [0.0, 1.0]);
    let [low, high] = wilson(10, 10);
    assert!(low > 0.7 && (high - 1.0).abs() < 1e-9);
}

#[test]
fn at_ninety_percent_five_of_ten_regresses_and_six_of_ten_does_not() {
    let low = gate_of(&history_at_ninety_percent(5), &[]);
    let task = only(&low);
    assert_eq!(task.status, Status::Regression);
    assert_eq!(task.baseline.map(|b| (b.passes, b.n)), Some((27, 30)));
    assert!(task.probability.unwrap() < ALPHA);
    assert!(low.failed());
    let fine = gate_of(&history_at_ninety_percent(6), &[]);
    assert_eq!(only(&fine).status, Status::Ok);
    assert!(!fine.failed());
}

#[test]
fn a_perfect_baseline_does_not_make_one_miss_a_regression_but_two_are() {
    let history = |window_passes: usize| {
        let mut all = Vec::new();
        for day in 20..30 {
            all.extend(night(day, "t", 3, 3));
        }
        all.extend(night(30, "t", window_passes, 10));
        all
    };
    let report = gate_of(&history(9), &[]);
    assert_eq!(only(&report).baseline.map(|b| b.rate), Some(1.0));
    assert_eq!(only(&report).status, Status::Ok);
    assert!(!report.failed());
    assert_eq!(only(&gate_of(&history(8), &[])).status, Status::Regression);
}

#[test]
fn a_regression_names_the_window_the_commits_and_the_transcripts_to_read() {
    let report = gate_of(&history_at_ninety_percent(5), &[]);
    let task = only(&report);
    assert_eq!(task.window_runs, [run_id(30, 0)]);
    assert_eq!(task.window_commits.len(), 1);
    assert_eq!(task.failing.len(), 5);
    assert!(task.failing[0].contains(&run_id(30, 0)));
    assert_eq!(task.passing.len(), 5);
    assert!(task.last_good_commit.as_deref().is_some_and(|c| c.starts_with("c-")));
}

#[test]
fn a_series_under_twenty_scored_attempts_is_provisional_and_never_flags() {
    // Fifteen passes, then a window of ten failures that would be a regression at once: nineteen scored attempts in all.
    let mut all = Vec::new();
    for day in 20..25 {
        all.extend(night(day, "t", 3, 3));
    }
    all.extend(night(30, "t", 0, 4));
    let report = gate_of(&all, &[]);
    let task = only(&report);
    assert_eq!((task.scored, task.status), (19, Status::Provisional));
    // The number is reported (the page shows it), but the verdict is not drawn from it.
    assert!(task.probability.is_some_and(|p| p < ALPHA));
    assert!(!report.failed());
    // The twentieth scored attempt makes it a series the gate may flag.
    all.extend(night(30, "t", 0, 1));
    let report = gate_of(&all, &[]);
    assert_eq!((only(&report).scored, only(&report).status), (20, Status::Regression));
    assert!(report.failed());
}

#[test]
fn attempts_that_are_not_scored_stay_out_of_the_rates() {
    let mut all = history_at_ninety_percent(5);
    // Errors, refusals and skips are not in the pass rate: ten of them do not make a window of passes and fails.
    let run = run_id(31, 0);
    for outcome in [AttemptOutcome::Error, AttemptOutcome::Refusal, AttemptOutcome::Skipped] {
        for _ in 0..10 {
            all.push(attempt(&run, "t", outcome));
        }
    }
    let report = gate_of(&all, &[]);
    let task = only(&report);
    assert_eq!((task.scored, task.window.n, task.window.passes), (40, 10, 5));
    assert_eq!(task.status, Status::Regression);
}

#[test]
fn without_a_baseline_in_the_twenty_eight_days_the_window_is_not_judged() {
    // The same thirty baseline attempts, but forty days before the window.
    let mut all = Vec::new();
    for day in 0..10 {
        all.extend(night(day, "t", 3 - usize::from(day < 3), 3));
    }
    all.extend(night(50, "t", 0, 10));
    let report = gate_of(&all, &[]);
    let task = only(&report);
    assert_eq!((task.status, task.baseline, task.probability), (Status::NoBaseline, None, None));
    assert!(!report.failed());
}

#[test]
fn series_are_split_by_version_model_and_effort_and_scripted_attempts_are_not_scored() {
    let mut all = history_at_ninety_percent(5);
    let mut other_model = night(30, "t", 0, 10);
    for record in &mut other_model {
        record.model = Some("n".to_owned());
    }
    let mut newer = night(30, "t", 0, 10);
    for record in &mut newer {
        record.task_version = 2;
    }
    let mut scripted = night(30, "t", 0, 10);
    for record in &mut scripted {
        record.driver = "scripted".to_owned();
    }
    all.extend(other_model);
    all.extend(newer);
    all.extend(scripted);
    let report = gate_of(&all, &[]);
    assert_eq!(report.tasks.len(), 3);
    let regressions: Vec<(u32, Option<&str>)> = report
        .tasks
        .iter()
        .filter(|t| t.status == Status::Regression)
        .map(|t| (t.version, t.model.as_deref()))
        .collect();
    assert_eq!(regressions, [(1, Some("m"))], "the other series are provisional");
}

#[test]
fn a_task_whose_baseline_is_under_half_is_weak_without_being_a_regression() {
    let mut all = Vec::new();
    for day in 20..30 {
        all.extend(night(day, "t", 1, 3));
    }
    all.extend(night(30, "t", 4, 10));
    let report = gate_of(&all, &[]);
    let task = only(&report);
    assert!(task.weak);
    assert_eq!(task.status, Status::Ok);
    assert!(!report.failed());
}

#[test]
fn a_suite_drop_of_six_points_flags_after_a_quiet_history_and_not_after_a_noisy_one() {
    let quiet = [0.90; 14];
    let (median, tolerance, regression) = suite_verdict(&quiet, 0.84).unwrap();
    assert!((median - 0.90).abs() < 1e-9 && (tolerance - FLOOR_POINTS).abs() < 1e-9);
    assert!(regression);
    assert!(!suite_verdict(&quiet, 0.86).unwrap().2, "four points is inside the floor");
    // Seven nights at 80 and seven at 96: the median is 88, the MAD 8 points, so the tolerance is 24 points.
    let noisy: Vec<f64> = (0..14).map(|i| if i % 2 == 0 { 0.80 } else { 0.96 }).collect();
    let (median, tolerance, regression) = suite_verdict(&noisy, 0.82).unwrap();
    assert!((median - 0.88).abs() < 1e-9 && (tolerance - 0.24).abs() < 1e-9, "{median} {tolerance}");
    assert!(!regression, "the same six-point drop is a normal bad night here");
    // Only the previous fourteen count, and fewer than fourteen cannot be judged.
    let mut long = vec![0.10; 5];
    long.extend(quiet);
    assert!(suite_verdict(&long, 0.84).unwrap().2);
    assert_eq!(suite_verdict(&quiet[..13], 0.0), None);
}

#[test]
fn the_latest_nightly_suite_is_judged_from_the_task_rates_of_that_night() {
    let mut all = Vec::new();
    let mut runs = Vec::new();
    for day in 0..15 {
        let passes = if day < 14 { 9 } else { 7 };
        all.extend(night(day, "t", passes, 10));
        runs.push(run_record(day, "m"));
    }
    let report = gate_of(&all, &runs);
    let [suite] = report.suites.as_slice() else { panic!("{:?}", report.suites) };
    assert_eq!((suite.model.as_str(), suite.run.as_str()), ("m", run_id(14, 0).as_str()));
    assert_eq!(suite.rate, Some(0.7));
    assert!((suite.median.unwrap() - 0.9).abs() < 1e-9);
    assert!(suite.regression);
    assert!(report.failed());
    // A model with too few nights is reported without a verdict.
    let few = gate_of(&all[..30], &runs[..3]);
    assert_eq!((few.suites[0].median, few.suites[0].regression), (None, false));
    // Only nightly reference runs make a suite.
    let mut weekly = runs.clone();
    for run in &mut weekly {
        run.kind = "weekly".to_owned();
    }
    assert_eq!(gate_of(&all, &weekly).suites.len(), 0);
}

#[test]
fn a_provisional_task_does_not_count_in_the_nights_suite_rate() {
    let mut all = night(0, "young", 0, 5);
    all.extend(night(0, "old", 4, 4));
    for day in 1..6 {
        all.extend(night(day, "old", 4, 4));
    }
    let runs = vec![run_record(0, "m")];
    let report = gate_of(&all, &runs);
    assert_eq!(report.suites[0].rate, Some(1.0), "`young` has five scored attempts and is left out");
}

#[test]
fn cost_per_attempt_up_by_a_quarter_at_the_same_pass_rate_is_a_warning() {
    let cost = |all: &mut Vec<AttemptRecord>, micros: u64| {
        for record in all.iter_mut().filter(|r| r.run == run_id(30, 0)) {
            record.usd_micros = micros;
        }
    };
    let mut all = history_at_ninety_percent(9);
    cost(&mut all, 1400);
    let report = gate_of(&all, &[]);
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].message.contains("1000 to 1400"), "{}", report.warnings[0].message);
    assert!(!report.failed(), "a warning never fails the gate");
    cost(&mut all, 1200);
    assert_eq!(gate_of(&all, &[]).warnings.len(), 0);
    // A fall in the pass rate is a different story from a heavier tool list.
    let mut worse = history_at_ninety_percent(5);
    cost(&mut worse, 2000);
    assert_eq!(gate_of(&worse, &[]).warnings.len(), 0);
}

#[test]
fn an_attempt_of_several_turns_that_read_nothing_from_the_cache_is_a_warning() {
    let mut all = history_at_ninety_percent(9);
    let last = all.len() - 1;
    all[last].usage.cache_read = 0;
    let one_turn = all.len() - 2;
    all[one_turn].usage.cache_read = 0;
    all[one_turn].turns = 1;
    let report = gate_of(&all, &[]);
    let warnings: Vec<&str> = report.warnings.iter().map(|w| w.message.as_str()).collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("cache") && warnings[0].contains("3 turns"));
}

#[test]
fn the_report_is_the_same_bytes_every_time() {
    let json = |report: &GateReport| serde_json::to_string(report).unwrap();
    let all = history_at_ninety_percent(5);
    assert_eq!(json(&gate_of(&all, &[])), json(&gate_of(&all, &[])));
    assert!(json(&gate_of(&all, &[])).contains("\"status\":\"regression\""));
}

#[test]
fn attempts_of_a_run_that_is_invalid_or_has_no_record_are_not_scored() {
    let mut all = history_at_ninety_percent(10);
    // Ten failing attempts a day later, in a run with too many errors: the gate would call it a regression.
    all.extend(night(31, "t", 0, 10));
    let invalid = RunRecord { valid: false, ..run_record(31, "m") };
    let report = gate_of(&all, &[invalid]);
    assert_eq!(only(&report).status, Status::Ok, "{:?}", only(&report));
    // The same attempts with no record at all (the process stopped before it wrote one) are left out as well.
    let runs: Vec<RunRecord> = (20..=30).map(|d| run_record(d, "m")).collect();
    let report = gate(&all, &runs);
    assert_eq!(only(&report).status, Status::Ok, "{:?}", only(&report));
    // With a valid record they count.
    let runs: Vec<RunRecord> = (20..=31).map(|d| run_record(d, "m")).collect();
    assert_eq!(only(&gate(&all, &runs)).status, Status::Regression);
}

#[test]
fn the_window_is_the_latest_attempts_by_time_not_by_where_they_sit_in_the_file() {
    // The night of day 30 passes ten of ten, but an older, slower run (day 5, all failing) appended after it.
    let mut all = history_at_ninety_percent(10);
    all.extend(night(5, "t", 0, 10));
    let report = gate_of(&all, &[]);
    assert_eq!(only(&report).status, Status::Ok, "{:?}", only(&report));
}

#[test]
fn an_invalid_last_night_does_not_hide_the_last_valid_one() {
    // Eleven quiet nights at 90%, then a night at 0% whose run was invalid (too many errors): the suite verdict is for
    // the last valid night, not "no verdict".
    let mut all = Vec::new();
    let mut runs = Vec::new();
    for day in 1..=11 {
        all.extend(night(day, "t", 9, 10));
        runs.push(run_record(day, "m"));
    }
    all.extend(night(12, "t", 0, 10));
    runs.push(RunRecord { valid: false, ..run_record(12, "m") });
    let report = gate_of(&all, &runs);
    assert_eq!(report.suites.len(), 1);
    assert_eq!(report.suites[0].run, run_id(11, 0), "{:?}", report.suites[0]);
    assert!(report.suites[0].rate.is_some());
}
