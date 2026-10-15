//! The check library (design 5): grades one attempt from the fixture, the result and the transcript.
//!
//! The caller opens two runners, one on a pristine copy of the fixture and one on the result folder, as the actor
//! `eval-checker` with `read` and `render` only. A grade is a pure function of those and the transcript, so grading
//! the same inputs twice gives the same bytes (ADR 0009).
//!
//! Names: a check may use the fixture's `exports` (such as `{main}`) and the task's `[bind]` names. The exports are
//! resolved where the fixture is built, so the caller hands the resolved map to [`Checker::with_exports`]; a name that
//! is both an export and a bind is a broken task and grades as an `error`.
//!
//! Not graded yet, and reported as an `error` outcome rather than skipped: the `doc.*` and `render.*` kinds and
//! `judge`.

mod bind;
mod gate;
mod log;
mod number;
mod query;
mod report;
mod resolve;
mod validate;
mod value;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use omuzaki_ops_api::Runner;
use serde_json::{Map, Value};

pub use self::report::{CheckReport, CheckResult, Outcome, Verdict};
use self::resolve::Names;
use super::task::{Check, CheckKind, Task};
use super::transcript::Transcript;

/// Grades the checks of a task against one attempt.
pub struct Checker {
    fixture: Arc<dyn Runner>,
    result: Arc<dyn Runner>,
    transcript: Transcript,
    /// The fixture's exports, already resolved to ids, by name.
    exports: BTreeMap<String, String>,
    /// "Before" values of read-only ops on the fixture, by op and arguments: the fixture does not change, so each is
    /// asked once per checker.
    before: Mutex<BTreeMap<String, Result<Value, Verdict>>>,
}

impl Checker {
    /// A checker over the runner on the pristine fixture, the runner on the result and the attempt's transcript.
    pub fn new(fixture: Arc<dyn Runner>, result: Arc<dyn Runner>, transcript: Transcript) -> Self {
        Self { fixture, result, transcript, exports: BTreeMap::new(), before: Mutex::new(BTreeMap::new()) }
    }

    /// The same checker, with the fixture's exports (`{main}` and the rest) resolved to ids for the checks to use.
    #[must_use]
    pub fn with_exports(mut self, exports: BTreeMap<String, String>) -> Self {
        self.exports = exports;
        self
    }

    /// Grades every check of `task`, in order.
    #[must_use]
    pub fn grade(&self, task: &Task) -> CheckReport {
        let bound = self.names(task);
        let results =
            task.checks.iter().map(|check| CheckResult::new(&check.id, check.hard, self.run(check, &bound))).collect();
        CheckReport { results }
    }

    /// Every name the checks of `task` may use: the fixture's exports, then the task's bindings.
    fn names(&self, task: &Task) -> Names {
        let mut names: Names = self.exports.iter().map(|(name, id)| (name.clone(), Ok(id.clone()))).collect();
        for (name, resolved) in bind::resolve(&*self.fixture, &*self.result, &task.bind) {
            let resolved = if names.contains_key(&name) {
                Err(Verdict::error(format!("`{name}` is both a fixture export and a [bind] name")))
            } else {
                resolved
            };
            names.insert(name, resolved);
        }
        names
    }

    /// One check, resolved first: a check that does not resolve is broken, and never reaches its grader.
    fn run(&self, check: &Check, bound: &Names) -> Verdict {
        let kind = match resolve::check(&check.kind, bound) {
            Ok(kind) => kind,
            Err(verdict) => return verdict,
        };
        match &kind {
            CheckKind::Query { op, args, at, expect } => self.query(op, args, at, expect),
            CheckKind::Gate { ops, declared } => self.gate(ops, *declared),
            CheckKind::LogApplied { op } => self.log_applied(op, true),
            CheckKind::LogNotApplied { op } => self.log_applied(op, false),
            CheckKind::LogWithin { ops } => self.log_within(ops),
            CheckKind::LogPreviewed {} => self.log_previewed(),
            CheckKind::LogMaxErrors { n } => self.log_max_errors(*n),
            other => Verdict::error(format!("check kind `{}` is not graded by this library yet", kind_name(other))),
        }
    }

    /// The output of a read-only op on the fixture, asked once per op and arguments.
    pub(super) fn before(&self, op: &str, args: &Map<String, Value>) -> Result<Value, Verdict> {
        let key = format!("{op} {}", Value::Object(args.clone()));
        let mut cache = self.before.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .entry(key)
            .or_insert_with(|| {
                value::call(&*self.fixture, op, args).map_err(|e| Verdict::error(format!("`{op}` on the fixture: {e}")))
            })
            .clone()
    }
}

fn kind_name(kind: &CheckKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}
