//! What the approval, budget, review and fence tests share: stand-in ops that ask a person (a scope agents lack, a
//! confirmation, a cost), and helpers that open a session with a narrow ceiling and read what a refusal queued.

use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_ops_api::{
    Actor, ApprovalId, CostHint, ErrorCode, ErrorDetail, Grants, Need, Op, OpCtx, OpError, OpKind, OpMeta, Plan,
    Request, Scope, SessionInfo, SessionSpec, Sessions, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Rig, request, session_spec};

/// Plans `value` at `path`, refusing a path whose parent is missing (so a dry-run can fail).
fn set_plan(ctx: &OpCtx<'_>, path: &DocPath, value: &Value) -> Result<Plan, OpError> {
    let (parent, _) = path.split_last().ok_or_else(|| OpError::invalid("path", "can't replace the root"))?;
    if ctx.doc.get(&parent).is_none() {
        return Err(OpError::not_found(format!("no parent {parent}")));
    }
    let before = ctx.doc.get(path).cloned();
    Ok(Plan::edit(Patch::new().with(path.clone(), before, Some(value.clone()))))
}

const fn edit(name: &'static str, summary: &'static str, scope: Scope) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope, undo: Undo::Auto }
}

/// An edit that asks a person to confirm it (`OpCtx::require_person`), like a model switch.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Confirm {
    pub path: DocPath,
    pub value: Value,
}

impl Op for Confirm {
    fn meta() -> OpMeta {
        edit("test.confirm", "Set a path once a person confirms.", Scope::EditCharacter)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("this switches the model")?;
        set_plan(ctx, &self.path, &self.value)
    }
}
register_op!(Confirm);

/// A publish that also asks a person to confirm: two needs, met one at a time.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct PublishConfirm {
    pub path: DocPath,
    pub value: Value,
}

impl Op for PublishConfirm {
    fn meta() -> OpMeta {
        edit("test.publish_confirm", "Publish once a person confirms.", Scope::Publish)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("this publishes outside the machine")?;
        set_plan(ctx, &self.path, &self.value)
    }
}
register_op!(PublishConfirm);

/// An edit only a person may make, which nobody can confirm for them.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct PersonOnly {
    pub path: DocPath,
}

impl Op for PersonOnly {
    fn meta() -> OpMeta {
        edit("test.person_only", "Bind a device (a person only).", Scope::EditSettings)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if matches!(ctx.actor, Actor::Person { .. }) {
            set_plan(ctx, &self.path, &Value::Bool(true))
        } else {
            Err(OpError::person_only("binding a device is done in Settings"))
        }
    }
}
register_op!(PersonOnly);

/// An edit that goes live (needs `broadcast`).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Broadcast {
    pub path: DocPath,
    pub value: Value,
}

impl Op for Broadcast {
    fn meta() -> OpMeta {
        edit("test.broadcast", "Go live.", Scope::Broadcast)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        set_plan(ctx, &self.path, &self.value)
    }
}
register_op!(Broadcast);

/// A paid edit: its plan costs `usd` dollars (0.12 unless given), or a broken cost when `bad` says so.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Paid {
    pub path: DocPath,
    pub value: Value,
    #[serde(default)]
    pub usd: Option<f32>,
    /// `nan`, `infinite` or `negative`: the op has a bug.
    #[serde(default)]
    pub bad: Option<String>,
}

impl Op for Paid {
    fn meta() -> OpMeta {
        edit("test.paid", "Set a path through a paid connector.", Scope::EditCharacter)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let usd = match self.bad.as_deref() {
            Some("nan") => f32::NAN,
            Some("infinite") => f32::INFINITY,
            Some("negative") => -0.5,
            _ => self.usd.unwrap_or(0.12),
        };
        let plan = set_plan(ctx, &self.path, &self.value)?;
        Ok(Plan { cost: Some(CostHint { usd: Some(usd), ..CostHint::default() }), ..plan })
    }
}
register_op!(Paid);

/// A free edit: no cost at all, or (with `zero`) a cost of exactly 0 dollars.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Free {
    pub path: DocPath,
    pub value: Value,
    #[serde(default)]
    pub zero: bool,
}

impl Op for Free {
    fn meta() -> OpMeta {
        edit("test.free", "Set a path through a free connector.", Scope::EditCharacter)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let plan = set_plan(ctx, &self.path, &self.value)?;
        let cost = self.zero.then(|| CostHint { usd: Some(0.0), ..CostHint::default() });
        Ok(Plan { cost, ..plan })
    }
}
register_op!(Free);

impl Rig {
    /// Opens a session for alice whose ceiling is `grants`, otherwise as `session_spec`.
    pub(crate) fn open_with(&self, grants: Grants) -> SessionInfo {
        self.exec.open(SessionSpec { grants, ..session_spec("claude") }).unwrap()
    }

    /// Opens a session for alice with the default agent ceiling: no `publish`, `broadcast` or `spend`.
    pub(crate) fn open_narrow(&self) -> SessionInfo {
        self.open_with(Grants::agent_default())
    }
}

/// A request to apply `op` as `actor`, sent with every scope (a transport that does not narrow): the session's ceiling
/// is what limits it.
pub(crate) fn wide(actor: &Actor, op: &str, args: Value) -> Request {
    Request { actor: actor.clone(), ..request(op, args) }
}

/// The approval a `Waiting` refusal queued, and what it waits for.
pub(crate) fn queued(error: &OpError) -> (ApprovalId, Need) {
    assert_eq!(error.code, ErrorCode::Waiting, "{error:?}");
    match error.detail.as_deref() {
        Some(ErrorDetail::Queued { approval, need }) => (*approval, need.clone()),
        other => panic!("not queued: {other:?}"),
    }
}
