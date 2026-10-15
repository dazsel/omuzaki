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
//! Not graded yet, and reported as an `error` outcome rather than skipped: `judge`.

mod bind;
mod doc;
mod docglob;
mod gate;
mod log;
mod number;
mod query;
mod render;
mod report;
mod resolve;
mod validate;
mod value;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use omuzaki_ops_api::Runner;
use omuzaki_store_api::Store;
use serde_json::{Map, Value};

pub use self::report::{CheckReport, CheckResult, Outcome, Verdict};
use self::resolve::Names;
use super::task::{Check, CheckKind, Task};
use super::transcript::{ToolCall, ToolNames, Transcript};

/// Grades the checks of a task against one attempt.
pub struct Checker {
    fixture: Arc<dyn Runner>,
    result: Arc<dyn Runner>,
    transcript: Transcript,
    /// How the transcript's tool names map to op names; without it the transcript must already use op names.
    tool_names: Option<ToolNames>,
    /// The transcript's calls in op names, built on first use (the `log.*` checks).
    calls: OnceLock<Result<Vec<ToolCall>, Verdict>>,
    /// The fixture's exports, already resolved to ids, by name.
    exports: BTreeMap<String, String>,
    /// "Before" values of read-only ops on the fixture, by op and arguments: the fixture does not change, so each is
    /// asked once per checker.
    before: Mutex<BTreeMap<String, Result<Value, Verdict>>>,
    /// The whole document of the fixture and of the result, each read once (the `doc.*` checks).
    fixture_doc: OnceLock<Result<Value, Verdict>>,
    result_doc: OnceLock<Result<Value, Verdict>>,
    /// The asset store the render ops write to, when the caller has one (the `render.*` checks).
    store: Option<Arc<dyn Store>>,
}

impl Checker {
    /// A checker over the runner on the pristine fixture, the runner on the result and the attempt's transcript.
    pub fn new(fixture: Arc<dyn Runner>, result: Arc<dyn Runner>, transcript: Transcript) -> Self {
        Self {
            fixture,
            result,
            transcript,
            tool_names: None,
            calls: OnceLock::new(),
            exports: BTreeMap::new(),
            before: Mutex::new(BTreeMap::new()),
            fixture_doc: OnceLock::new(),
            result_doc: OnceLock::new(),
            store: None,
        }
    }

    /// The same checker, reading the transcript's tool names through `names` (the server's catalog).
    #[must_use]
    pub fn with_tool_names(mut self, names: ToolNames) -> Self {
        self.tool_names = Some(names);
        self
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
            CheckKind::DocEquals { path, value } => self.doc_equals(path, value),
            CheckKind::DocRange { path, min, max } => self.doc_range(path, *min, *max),
            CheckKind::DocExists { path } => self.doc_exists(path, true),
            CheckKind::DocAbsent { path } => self.doc_exists(path, false),
            CheckKind::DocChange { path, change, at_least, relative } => {
                self.doc_change(path, *change, *at_least, *relative)
            }
            CheckKind::DocChangedOnly { paths } => self.doc_changed_only(paths),
            CheckKind::DocUnchanged { paths } => self.doc_unchanged(paths),
            CheckKind::RenderDiff { render, region, expect } => self.render_diff(render, region, expect),
            CheckKind::RenderSame { render } => self.render_same(render),
            CheckKind::Query { op, args, at, expect } => self.query(op, args, at, expect),
            CheckKind::Gate { ops, declared } => self.gate(ops, *declared),
            CheckKind::LogApplied { op } => self.log_applied(op, true),
            CheckKind::LogNotApplied { op } => self.log_applied(op, false),
            CheckKind::LogWithin { ops } => self.log_within(ops),
            CheckKind::LogPreviewed {} => self.log_previewed(),
            CheckKind::LogMaxErrors { n } => self.log_max_errors(*n),
            other @ CheckKind::Judge { .. } => {
                Verdict::error(format!("check kind `{}` is not graded by this library yet", kind_name(other)))
            }
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
