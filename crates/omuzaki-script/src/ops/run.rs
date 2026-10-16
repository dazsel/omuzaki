//! `script.run`: run a script, or replay a saved macro, as a driver job.

use omuzaki_core::id::Slug;
use omuzaki_core::time::{Clock, SystemClock};
use omuzaki_doc_api::{Patch, Value};
use omuzaki_ops_api::{
    CostHint, ErrorDetail, Grants, JobCtx, JobShape, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo,
    register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};

use crate::bindings::refusal_registered;
use crate::error::limit_error;
use crate::job::JobScript;
use crate::saved::{Body, Item, Place, item_path, not_saved, store};
use crate::{
    LimitKind, Limits, RecordedCall, RunRequest, ScriptError, ScriptErrorKind, registered_host,
};

/// Seconds a run gets when the caller doesn't say.
const DEFAULT_SECONDS: u32 = 120;

/// Runs a script (`source`) or a saved script or macro (`id`) as a job: one transaction, so one undo step, rolled back
/// whole on the first error, a limit or a cancel. `params` is the map the script reads as `params`; `dry_run` sends
/// every inner call as a dry run and lists the plans instead of changing anything (the calls see the document as it was
/// before the script, so later calls that read earlier ones' writes may plan differently); `seconds` is the wall clock,
/// 120 by default and at most 600.
///
/// The answer is the run's [`ScriptOutcome`](crate::ScriptOutcome) as JSON: `value` (the script's last value), `output`
/// (what it printed), `calls` and, for a dry run, `plans`. A source over 64 KiB, or one that [`script.check`] reports
/// errors for, is refused before anything starts, with `Invalid`, the field `source` and the first error's line and
/// column (in the detail and at the end of the message). A failed inner call ends the run with that call's own error.
///
/// It needs the `script` scope, which no default agent ceiling holds. Each inner call is checked against the job's
/// grants, so a run can't do what its caller couldn't; the script's `// omuzaki: grants = [...]` line can only narrow
/// that further. A script can't call `script.run`.
///
/// [`script.check`]: crate::ops::check::ScriptCheck
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptRun {
    /// The Rhai source to run (at most 64 KiB). Give this or `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The id of a saved script or macro, looked up in the project and then in My scripts. Give this or `source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Slug>,
    /// What the script reads as `params`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Map<String, Value>>,
    /// Plan every inner call instead of applying it.
    #[serde(default)]
    pub dry_run: bool,
    /// The wall-clock limit in seconds: 1 to 600, 120 if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u32>,
}

/// What `plan` resolved, passed to `work` as the plan's output.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Planned {
    Script { name: String, source: String },
    Macro { name: String, steps: Vec<RecordedCall> },
}

impl ScriptRun {
    /// Runs `source` on the engine over the job's calls.
    fn run_script(&self, job: &JobCtx<'_>, name: String, source: String) -> Result<Value, OpError> {
        let ops = crate::registered::shared_ops();
        let host = registered_host(Limits::job(self.seconds));
        let calls = JobScript::new(job, &ops);
        let request = RunRequest {
            name,
            source,
            actor: job.actor().clone(),
            // The executor checks every inner call against the job's grants; the script's own narrowing is applied to
            // these and enforced by the adapter.
            grants: Grants::person(),
            tx: None,
            params: self.params.clone().unwrap_or_default(),
            dry_run: self.dry_run,
        };
        let outcome = host.run(request, &calls, &calls).map_err(|e| run_error(job, &e))?;
        serde_json::to_value(outcome).map_err(|e| OpError::internal(format!("serialising the outcome failed: {e}")))
    }

    fn seconds(&self) -> Result<u32, OpError> {
        let seconds = self.seconds.unwrap_or(DEFAULT_SECONDS);
        if (1..=Limits::MAX_JOB_SECONDS).contains(&seconds) {
            Ok(seconds)
        } else {
            Err(OpError::invalid("seconds", format!("a run gets 1 to {} seconds", Limits::MAX_JOB_SECONDS))
                .hint(format!("leave `seconds` out for {DEFAULT_SECONDS}")))
        }
    }

    /// The script or macro to run, found and checked.
    fn resolve(&self, ctx: &OpCtx<'_>) -> Result<Item, OpError> {
        match (&self.source, &self.id) {
            (Some(source), None) => {
                let body = Body::validated(Some(source), None)?;
                let hash = crate::content_hash(&body.text(""));
                Ok(Item { id: String::new(), name: "script".to_owned(), body, hash, author: None })
            }
            (None, Some(id)) => find(ctx, id.as_str()),
            _ => Err(OpError::invalid("source", "give exactly one of `source` and `id`")
                .hint("`source` runs Rhai text; `id` runs something saved with `script.save`")),
        }
    }
}

/// The item saved as `id`: in the project first, then in My scripts when a store is wired.
fn find(ctx: &OpCtx<'_>, id: &str) -> Result<Item, OpError> {
    if let Some(node) = ctx.doc.get(&item_path(id)) {
        return Item::from_node(id, node).map_err(|problem| {
            OpError::invalid("id", format!("`{id}` is saved in the project but can't be read: {problem}"))
        });
    }
    if let Some(stored) = store(ctx).ok().map(|store| store.get(id)).transpose()?.flatten() {
        return Item::from_stored(&stored)
            .map_err(|problem| OpError::invalid("id", format!("`{id}` is saved in My scripts but can't be read: {problem}")));
    }
    Err(not_saved(id, Place::Project).hint("list what is saved with `script.list`, or pass the text as `source`"))
}

/// Refuses a script the check finds a problem with, at the first problem's position.
fn require_clean(source: &str) -> Result<(), OpError> {
    let report = registered_host(Limits::console()).check(source);
    match report.first_problem() {
        None => Ok(()),
        Some(problem) => Err(OpError::invalid(
            "source",
            format!("{} (line {}, column {})", problem.message, problem.line, problem.column),
        )
        .hint("run `script.check` for every problem")
        .with_detail(ErrorDetail::Position { line: problem.line, column: problem.column })),
    }
}

impl Op for ScriptRun {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.run",
            summary: "Run a script or a saved macro as one undoable step.",
            kind: OpKind::Job,
            scope: Scope::Script,
            undo: Undo::Auto,
        }
    }

    /// Validates, resolves what to run and returns an empty patch: the run's edits are made by its inner calls. The
    /// resolved script rides in the plan's output, which `work` reads back.
    #[allow(clippy::cast_precision_loss)] // the cost hint is an estimate, and `seconds` is at most 600
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let seconds = self.seconds()?;
        let item = self.resolve(ctx)?;
        let planned = match item.body {
            Body::Source(source) => {
                require_clean(&source)?;
                Planned::Script { name: item.name, source }
            }
            Body::Steps(steps) => Planned::Macro { name: item.name, steps },
        };
        let resolved = serde_json::to_value(planned)
            .map_err(|e| OpError::internal(format!("serialising the resolved script failed: {e}")))?;
        Ok(Plan {
            patch: Patch::new(),
            output: Some(resolved),
            cost: Some(CostHint { seconds: Some(seconds as f32), ..CostHint::default() }),
            effect: None,
        })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let planned = job
            .plan_output()
            .ok_or_else(|| OpError::internal("script.run was started without the script its plan resolved"))?;
        let planned: Planned = serde_json::from_value(planned.clone())
            .map_err(|e| OpError::internal(format!("the planned script does not read back: {e}")))?;
        match planned {
            Planned::Script { name, source } => self.run_script(job, name, source),
            Planned::Macro { steps, .. } => replay(job, &steps, self.dry_run, self.seconds.unwrap_or(DEFAULT_SECONDS)),
        }
    }

    fn job_shape() -> JobShape {
        JobShape::Driver
    }
}

/// A macro's steps through the job, one inner request each, stopping at the first failure or cancel. A replay is held
/// to what a script is: a step a script couldn't call (a job, an edit with outside effects, `script.run`) is refused
/// before anything is sent, at most [`Limits::calls`] steps run, and the clock stops it after `seconds`.
#[allow(clippy::cast_precision_loss)] // a progress fraction
fn replay(job: &JobCtx<'_>, steps: &[RecordedCall], dry_run: bool, seconds: u32) -> Result<Value, OpError> {
    let limits = Limits::job(Some(seconds));
    if steps.len() > limits.calls as usize {
        return Err(limit_error(
            LimitKind::Calls,
            format!("the macro has {} steps, over the {}-call limit", steps.len(), limits.calls),
        ));
    }
    for (index, step) in steps.iter().enumerate() {
        if let Some(refusal) = refusal_registered(&step.op) {
            let mut error = step_failed(index, step, refusal);
            error.hint = error.hint.or_else(|| Some("run it on its own, outside the macro".to_owned()));
            return Err(error);
        }
    }
    let mode = if dry_run { Mode::DryRun } else { Mode::Apply };
    let clock = SystemClock::new();
    let started = clock.now().0;
    let mut plans = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        job.checkpoint("macro", index as f32 / steps.len() as f32)?;
        let outcome = job.call(&step.op, step.args.clone(), mode).map_err(|error| step_failed(index, step, error))?;
        // After the call, not before it: a call that crosses the deadline (the last one included) must not end in a
        // success that commits after the time the caller was promised.
        if clock.now().0 - started > f64::from(seconds) {
            return Err(limit_error(LimitKind::Time, format!("the macro ran over its {seconds}-second limit")));
        }
        if dry_run {
            plans.push(json!({ "op": step.op, "args": step.args, "plan": outcome.plan }));
        }
    }
    Ok(json!({ "value": null, "output": [], "calls": steps.len(), "tx": null, "plans": plans }))
}

/// `error` as the failure of step `index` (0-based), named by its 1-based number and op.
fn step_failed(index: usize, step: &RecordedCall, mut error: OpError) -> OpError {
    error.message = format!("step {} (`{}`) failed: {}", index + 1, step.op, error.message);
    error
}

/// How a failed run reaches the caller: a cancel as the job's cancellation, anything else as
/// [`ScriptError::to_op_error`] says.
fn run_error(job: &JobCtx<'_>, error: &ScriptError) -> OpError {
    if error.kind == ScriptErrorKind::Cancelled { OpError::cancelled(job.id()) } else { error.to_op_error() }
}

register_op!(ScriptRun);
