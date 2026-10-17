//! The eval history (design 7.1, 7.2): one JSON line per attempt in `status/eval-history.jsonl` and one per run in
//! `status/eval-runs.jsonl`, appended only, schema `v: 1`.
//!
//! Appends are atomic per line: the file is opened in append mode and the whole line is handed to the OS in one
//! `write`, so two runs on one machine cannot interleave their lines. The gate (`eval gate`) reads the history back
//! through [`read_attempts`], which fails on a line it cannot read rather than skipping it: a gate that silently drops
//! records would call a regression quiet.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::check::{CheckReport, Outcome as CheckOutcome};

/// The schema version of every record.
pub const SCHEMA: u32 = 1;
/// The attempts file, in the status folder.
pub const HISTORY_FILE: &str = "eval-history.jsonl";
/// The runs file, in the status folder.
pub const RUNS_FILE: &str = "eval-runs.jsonl";
/// The only family the gate scores today; model suites and judges are recorded by their own tasks.
pub const FAMILY_TASK: &str = "task";

/// How an attempt ended (design 7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// Every hard check passed.
    Pass,
    /// A hard check failed, or the agent hit a turn, tool-call, token or money cap.
    Fail,
    /// The model refused. Not in the pass rate.
    Refusal,
    /// API failure, wall-clock timeout, a checker or harness fault. Not in the pass rate.
    Error,
    /// Not run because a maintainer decision is pending. Not in the pass rate.
    Skipped,
}

impl AttemptOutcome {
    /// Whether the outcome counts in a pass rate: `pass` and `fail` do.
    #[must_use]
    pub fn is_scored(self) -> bool {
        matches!(self, Self::Pass | Self::Fail)
    }

    /// The outcome as the history spells it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Refusal => "refusal",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

/// One check, as the history keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckRecord {
    /// The check's id.
    pub id: String,
    /// Whether it decides the attempt.
    pub hard: bool,
    /// For a hard check: whether it passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,
    /// For a soft check: its score in 0..=1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// For a check that errored: why. Harness trouble, never scored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The value on the fixture, when the check looked at one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Value>,
    /// The value on the result, when the check looked at one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
}

impl CheckRecord {
    /// The records of a graded attempt, in the task's order.
    #[must_use]
    pub fn of_report(report: &CheckReport) -> Vec<Self> {
        report
            .results
            .iter()
            .map(|r| Self {
                id: r.id.clone(),
                hard: r.hard,
                passed: (r.hard && r.outcome != CheckOutcome::Error).then_some(r.outcome == CheckOutcome::Pass),
                score: r.score,
                error: (r.outcome == CheckOutcome::Error).then(|| r.detail.clone()),
                before: r.before.clone(),
                after: r.after.clone(),
            })
            .collect()
    }
}

/// Token usage of an attempt; zeros for a driver that calls no model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Tokens read from the prompt cache.
    pub cache_read: u64,
    /// Tokens written to the prompt cache.
    pub cache_write: u64,
}

/// One line of `eval-history.jsonl` (design 7.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptRecord {
    /// [`SCHEMA`].
    pub v: u32,
    /// The run's id.
    pub run: String,
    /// The commit the run was made at.
    pub commit: String,
    /// [`FAMILY_TASK`], `model` or `judge`.
    pub family: String,
    /// `scripted` or `reference`.
    pub driver: String,
    /// The model, for the reference driver.
    pub model: Option<String>,
    /// The effort, for the reference driver.
    pub effort: Option<String>,
    /// The task's id.
    pub task: String,
    /// The task's version.
    pub task_version: u32,
    /// The task's content hash.
    pub task_hash: String,
    /// The spec feature the task belongs to.
    pub feature: String,
    /// The attempt number within the run, from 1.
    pub attempt: u32,
    /// How it ended.
    pub outcome: AttemptOutcome,
    /// Why, for anything but a pass.
    pub reason: Option<String>,
    /// The checks.
    pub checks: Vec<CheckRecord>,
    /// Model turns.
    pub turns: u64,
    /// Tool calls.
    pub tool_calls: u64,
    /// Tool calls the server answered with an error.
    pub tool_errors: u64,
    /// Request retries.
    pub retries: u64,
    /// Token usage.
    pub usage: Usage,
    /// Spend in millionths of a dollar.
    pub usd_micros: u64,
    /// Wall-clock milliseconds.
    pub wall_ms: u64,
    /// Hash of the tool list the server offered.
    pub tools_hash: String,
    /// Hash of the task's prompt.
    pub prompt_hash: String,
    /// The harness version.
    pub harness: String,
    /// Where the transcript and check report are, relative to the evals folder.
    pub transcript: String,
    /// For the scripted driver: the script that was played.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

/// How many attempts ended each way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Passed.
    pub pass: u64,
    /// Failed.
    pub fail: u64,
    /// Refused.
    pub refusal: u64,
    /// Errored.
    pub error: u64,
    /// Skipped.
    pub skipped: u64,
}

impl Counts {
    /// Counts one more attempt that ended as `outcome`.
    pub fn add(&mut self, outcome: AttemptOutcome) {
        match outcome {
            AttemptOutcome::Pass => self.pass += 1,
            AttemptOutcome::Fail => self.fail += 1,
            AttemptOutcome::Refusal => self.refusal += 1,
            AttemptOutcome::Error => self.error += 1,
            AttemptOutcome::Skipped => self.skipped += 1,
        }
    }

    /// All attempts.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.pass + self.fail + self.refusal + self.error + self.skipped
    }
}

/// A task a run did not run, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    /// The task.
    pub task: String,
    /// Why: a refused fixture, a cap, a maintainer decision.
    pub why: String,
}

/// The expected scripts a scripted run played besides its recorded attempts (the oracles of design 5.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Oracles {
    /// Scripts played: pass scripts, fail scripts and nulls.
    pub played: u64,
    /// Those that did not end as they must.
    pub deviations: u64,
}

/// What a run spent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    /// Millionths of a dollar; always 0 for the scripted driver.
    pub usd_micros: u64,
}

/// One line of `eval-runs.jsonl` (design 7.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    /// [`SCHEMA`].
    pub v: u32,
    /// The run's id.
    pub run: String,
    /// `pr`, `nightly`, `weekly` or `manual`.
    pub kind: String,
    /// `scripted` or `reference`.
    pub driver: String,
    /// The model, for the reference driver.
    pub model: Option<String>,
    /// The commit.
    pub commit: String,
    /// The tasks that ran.
    pub tasks_run: Vec<String>,
    /// The tasks that did not, with why.
    pub tasks_skipped: Vec<Skipped>,
    /// Recorded attempts by outcome.
    pub attempts: Counts,
    /// The oracle scripts played.
    pub oracles: Oracles,
    /// What it spent.
    pub spend: Spend,
    /// Wall-clock milliseconds.
    pub wall_ms: u64,
    /// `false` when more than 10% of the attempts errored: the run's rates are not to be trusted.
    pub valid: bool,
}

/// The share of errored attempts above which a run is not valid.
pub const MAX_ERROR_SHARE: f64 = 0.10;

/// Whether a run with these counts is valid: no more than [`MAX_ERROR_SHARE`] of its attempts errored.
#[must_use]
pub fn valid(counts: &Counts) -> bool {
    // Compared as integers: `error / total <= 1/10`.
    counts.error * 10 <= counts.total()
}

/// Why the history could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// A file could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// A line is not a record this harness understands.
    #[error("{}:{line}: {reason}", path.display())]
    Line {
        /// The file.
        path: PathBuf,
        /// 1-based line number.
        line: usize,
        /// What is wrong.
        reason: String,
    },
}

/// Appends `record` to `path` as one line, creating the file and its folder if need be.
///
/// # Errors
/// [`HistoryError::Io`] when the file cannot be opened or written.
pub fn append<T: Serialize>(path: &Path, record: &T) -> Result<(), HistoryError> {
    let io_err = |source| HistoryError::Io { path: path.to_owned(), source };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let mut line = serde_json::to_string(record).map_err(|e| io_err(io::Error::other(e)))?;
    line.push('\n');
    // One `write` of the whole line to a file in append mode: the OS places it after any line another process added.
    let mut file = OpenOptions::new().create(true).append(true).open(path).map_err(io_err)?;
    file.write_all(line.as_bytes()).map_err(io_err)
}

/// Every task attempt in the history at `path`, oldest first; other families are left for their own readers. A missing
/// file is an empty history.
///
/// # Errors
/// [`HistoryError`] for a file that cannot be read, a line that is not JSON, or a record of another schema version.
pub fn read_attempts(path: &Path) -> Result<Vec<AttemptRecord>, HistoryError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(HistoryError::Io { path: path.to_owned(), source }),
    };
    let mut attempts = Vec::new();
    for (index, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let bad = |reason: String| HistoryError::Line { path: path.to_owned(), line: index + 1, reason };
        let value: Value = serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
        if value.get("v").and_then(Value::as_u64) != Some(u64::from(SCHEMA)) {
            return Err(bad(format!("not a record of schema v{SCHEMA}")));
        }
        if value.get("family").and_then(Value::as_str) == Some(FAMILY_TASK) {
            attempts.push(serde_json::from_value(value).map_err(|e| bad(e.to_string()))?);
        }
    }
    Ok(attempts)
}

/// Every run in `path`, oldest first. A missing file is an empty list.
///
/// # Errors
/// As [`read_attempts`].
pub fn read_runs(path: &Path) -> Result<Vec<RunRecord>, HistoryError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(HistoryError::Io { path: path.to_owned(), source }),
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(index, line)| {
            let bad = |reason: String| HistoryError::Line { path: path.to_owned(), line: index + 1, reason };
            let run: RunRecord = serde_json::from_str(line).map_err(|e| bad(e.to_string()))?;
            if run.v == SCHEMA { Ok(run) } else { Err(bad(format!("not a record of schema v{SCHEMA}"))) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(pass: u64, fail: u64, error: u64) -> Counts {
        Counts { pass, fail, error, ..Counts::default() }
    }

    #[test]
    fn a_run_is_valid_up_to_ten_percent_errors() {
        assert!(valid(&counts(9, 0, 1)));
        assert!(!valid(&counts(8, 1, 2)));
        assert!(valid(&counts(0, 0, 0)));
        assert!(!valid(&counts(0, 0, 1)));
    }

    #[test]
    fn outcomes_count_and_only_pass_and_fail_are_scored() {
        let mut counts = Counts::default();
        for outcome in [AttemptOutcome::Pass, AttemptOutcome::Pass, AttemptOutcome::Error, AttemptOutcome::Refusal] {
            counts.add(outcome);
        }
        assert_eq!((counts.pass, counts.error, counts.refusal, counts.total()), (2, 1, 1, 4));
        assert!(AttemptOutcome::Fail.is_scored() && !AttemptOutcome::Error.is_scored());
        assert_eq!(serde_json::to_string(&AttemptOutcome::Refusal).unwrap(), "\"refusal\"");
    }
}
