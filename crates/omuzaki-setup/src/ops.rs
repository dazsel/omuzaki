//! `setup.*` (`docs/design/first-run.md` §2, §4 and §7, #988): where first-run setup stands, and the three changes a
//! menu or an agent makes to it. The progress lives in the session (`SessionState.setup`), written at once.
//!
//! | Op | For an agent | Undone by |
//! |---|---|---|
//! | `setup.status` | allowed | (a query) |
//! | `setup.skip` | waits for a person | `setup.restore` with the progress before |
//! | `setup.finish` | waits for a person | `setup.restore` |
//! | `setup.restart` (Help, Run setup again) | a person's alone | `setup.restore` |
//! | `setup.restore` | waits for a person | `setup.restore` |
//!
//! None of them changes a setting, a project, a calibration or an output: Run setup again only moves the flow back to
//! Welcome, and every step then shows the current choice.

use std::sync::Arc;

use omuzaki_ops_api::{Actor, Effect, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_settings_api::service::Settings;
use omuzaki_settings_api::session::{SessionState, SetupProgress};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::progress::{Progress, SetupStore, Step, StoreError, TOTAL_BUDGET_S};

/// The app's settings service, which holds the session.
fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn Settings, OpError> {
    ctx.service::<dyn Settings>()
}

const fn query(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}

const fn edit(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::EditSettings, undo: Undo::Explicit }
}

/// Where first-run setup stands: the step, what was skipped, whether it ended, and each step's time budget.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

/// End first-run setup now, keeping every choice already made (Skip setup). An agent's call waits for a person.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Skip {}

/// Finish first-run setup from the Go-live check. An agent's call waits for a person.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Finish {}

/// Run setup again from Welcome (Help, Run setup again). No setting, project, calibration or output changes. A
/// person's call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Restart {}

/// Put setup's progress back as it was: the undo of the other `setup.*` edits. `null` is "never decided".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "progress": { "step": "input", "skipped": ["calibrate"], "finished": false } },
    { "progress": null }
]))]
pub struct Restore {
    /// The progress to put back.
    pub progress: Option<SetupProgress>,
}

/// The step `progress` names; an id this build does not know is Welcome (the flow resumes there).
fn step_of(progress: Option<&SetupProgress>) -> Step {
    progress.and_then(|p| Step::from_id(&p.step)).unwrap_or(Step::Welcome)
}

fn status_json(state: &SessionState) -> Value {
    let progress = state.setup.as_ref();
    let step = step_of(progress);
    let budgets: serde_json::Map<String, Value> =
        Step::ALL.iter().map(|s| (s.id().to_owned(), json!(s.budget_s()))).collect();
    json!({
        "step": step.id(),
        "position": step.index(),
        "steps": Step::ALL.iter().map(|s| s.id()).collect::<Vec<_>>(),
        "finished": progress.is_some_and(|p| p.finished),
        "needed": state.setup_needed(),
        "decided": progress.is_some(),
        "skipped_steps": progress.map(|p| p.skipped.clone()).unwrap_or_default(),
        "budget_s_by_step": budgets,
        "total_budget_s": TOTAL_BUDGET_S,
    })
}

fn restore_call(progress: Option<&SetupProgress>) -> OpCall {
    OpCall { op: "setup.restore".to_owned(), args: json!({ "progress": progress }) }
}

fn call(op: &str) -> OpCall {
    OpCall { op: op.to_owned(), args: json!({}) }
}

/// An agent's change waits for a person: setup is the person's own first sitting.
fn may_change(ctx: &OpCtx<'_>, what: &str) -> Result<(), OpError> {
    if matches!(ctx.actor, Actor::Person { .. }) { Ok(()) } else { ctx.require_person(what.to_owned()) }
}

/// Writes `next` (unless it is a dry run, or already there) and answers with the status after, undone by putting
/// `before` back.
fn change(ctx: &OpCtx<'_>, next: Option<SetupProgress>, summary: &str, redo: OpCall) -> Result<Plan, OpError> {
    let session = service(ctx)?.session();
    let before = session.state();
    if before.setup == next {
        return Ok(Plan::answer(json!({ "changed": false, "status": status_json(&before) })));
    }
    let mut after = before.clone();
    after.setup.clone_from(&next);
    if !ctx.dry_run {
        session.set_setup(next).map_err(|e| OpError::internal(e.to_string()))?;
    }
    let effect = Effect { summary: summary.to_owned(), undo: restore_call(before.setup.as_ref()), redo };
    let output = json!({ "changed": true, "status": status_json(&after), "applied": !ctx.dry_run });
    Ok(Plan { effect: Some(effect), ..Plan::answer(output) })
}

impl Op for Status {
    fn meta() -> OpMeta {
        query(
            "setup.status",
            "Read where first-run setup stands: the step, skipped steps, whether it ended, and each step's budget.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(status_json(&service(ctx)?.session().state())))
    }
}

impl Op for Skip {
    fn meta() -> OpMeta {
        edit("setup.skip", "End first-run setup now, keeping what was chosen; check setup.status after.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        may_change(ctx, "skip first-run setup")?;
        let state = service(ctx)?.session().state();
        if state.setup.as_ref().is_some_and(|p| p.finished) {
            return Ok(Plan::answer(json!({ "changed": false, "status": status_json(&state) })));
        }
        let progress = state.setup.as_ref();
        let ended =
            SetupProgress::ended(step_of(progress).id(), progress.map(|p| p.skipped.clone()).unwrap_or_default());
        change(ctx, Some(ended), "skipped first-run setup", call("setup.skip"))
    }
}

impl Op for Finish {
    fn meta() -> OpMeta {
        edit("setup.finish", "Finish first-run setup from the Go-live check; setup.status says which step it is on.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        may_change(ctx, "finish first-run setup")?;
        let state = service(ctx)?.session().state();
        let progress = state.setup.as_ref();
        if progress.is_some_and(|p| p.finished) {
            return Ok(Plan::answer(json!({ "changed": false, "status": status_json(&state) })));
        }
        let step = step_of(progress);
        if step != Step::GoLive {
            return Err(OpError::conflict(format!("setup is on the {} step, not the Go-live check", step.id()))
                .hint("finish from the Go-live check, or end it now with setup.skip"));
        }
        let skipped = progress.map(|p| p.skipped.clone()).unwrap_or_default();
        change(ctx, Some(SetupProgress::ended(step.id(), skipped)), "finished first-run setup", call("setup.finish"))
    }
}

impl Op for Restart {
    fn meta() -> OpMeta {
        edit("setup.restart", "Run first-run setup again from Welcome; no setting, project or output changes.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !matches!(ctx.actor, Actor::Person { .. }) {
            return Err(OpError::person_only("only a person may run setup again: it is their Help menu"));
        }
        change(ctx, Some(SetupProgress::start()), "started first-run setup again", call("setup.restart"))
    }
}

impl Op for Restore {
    fn meta() -> OpMeta {
        edit("setup.restore", "Put first-run setup's progress back as it was (the undo of the other setup ops).")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        may_change(ctx, "change first-run setup's progress")?;
        change(ctx, self.progress.clone(), "put first-run setup back", restore_call(self.progress.as_ref()))
    }
}

register_op!(Status);
register_op!(Skip);
register_op!(Finish);
register_op!(Restart);
register_op!(Restore);

/// The progress as the session holds it, by step id.
impl From<&Progress> for SetupProgress {
    fn from(progress: &Progress) -> Self {
        let skipped = progress.skipped.iter().map(|s| s.id().to_owned()).collect();
        SetupProgress::new(progress.step.id(), skipped, progress.finished)
    }
}

/// The progress the flow resumes from: an unknown step id is Welcome and an unknown skipped id is dropped.
impl From<&SetupProgress> for Progress {
    fn from(progress: &SetupProgress) -> Self {
        let mut skipped: Vec<Step> = progress.skipped.iter().filter_map(|id| Step::from_id(id)).collect();
        skipped.sort_unstable();
        skipped.dedup();
        Progress { step: Step::from_id(&progress.step).unwrap_or(Step::Welcome), skipped, finished: progress.finished }
    }
}

/// The [`SetupStore`] over the person's session (`SessionState.setup`), written at once: what the studio gives the
/// flow, so the Setup panel, `setup.status` and an agent all see one progress.
pub struct SessionSetupStore(pub Arc<dyn Settings>);

impl SetupStore for SessionSetupStore {
    fn load(&self) -> Progress {
        self.0.session().state().setup.as_ref().map(Progress::from).unwrap_or_default()
    }

    fn save(&self, progress: &Progress) -> Result<(), StoreError> {
        self.0.session().set_setup(Some(SetupProgress::from(progress))).map_err(|e| StoreError::new(e.to_string()))
    }
}
