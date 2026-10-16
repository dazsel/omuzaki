//! The `trigger.*` ops (PF-25): bind a key to a face expression, unbind it, press it and list the bindings. A key is only
//! a name here; the OS key capture that calls `trigger.press` is a later task.

use omuzaki_anim::{MAX_TRIGGERS, TriggerMode};
use omuzaki_core::text::nearest;
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{Effect, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{host, op_error};
use crate::perform::host::PerformHost;
use crate::perform::triggers::{TriggerBinding, TriggerInfo, check_key, same_key};

fn default_mode() -> TriggerMode {
    TriggerMode::Toggle
}

fn default_down() -> bool {
    true
}

/// The answer `value` serialises to.
fn answer(value: impl Serialize) -> Result<Plan, OpError> {
    let value = serde_json::to_value(value).map_err(|e| OpError::internal(e.to_string()))?;
    Ok(Plan::answer(value))
}

/// The binding `key` has now, if any: what an undo puts back.
fn binding_of(host: &dyn PerformHost, key: &str) -> Option<TriggerBinding> {
    host.triggers().into_iter().map(|info| info.binding).find(|binding| same_key(&binding.key, key))
}

/// `name` for `mode`, as the ops spell it.
fn mode_name(mode: TriggerMode) -> &'static str {
    match mode {
        TriggerMode::Toggle => "toggle",
        TriggerMode::Hold => "hold",
    }
}

/// The call that binds `binding`: the redo of a bind and the undo of an unbind.
fn bind_call(binding: &TriggerBinding) -> Result<OpCall, OpError> {
    let args = Bind { key: binding.key.clone(), expression: binding.expression.as_str().to_owned(), mode: binding.mode };
    call("trigger.bind", &args)
}

/// The call that unbinds `key`.
fn unbind_call(key: &str) -> Result<OpCall, OpError> {
    call("trigger.unbind", &Unbind { key: key.to_owned() })
}

/// `op` with `args` serialised, as a stored call.
fn call(op: &str, args: &impl Serialize) -> Result<OpCall, OpError> {
    let args = serde_json::to_value(args).map_err(|e| OpError::internal(e.to_string()))?;
    Ok(OpCall { op: op.to_owned(), args })
}

/// The answer `value` serialises to, with the `effect` the op had outside the document, so the step can be undone.
fn answer_with(value: impl Serialize, effect: Option<Effect>) -> Result<Plan, OpError> {
    let mut plan = answer(value)?;
    plan.effect = effect;
    Ok(plan)
}

/// Bind a key to a face expression: pressing the key shows the expression, toggled or only while held. Binding a key
/// that is already bound replaces its binding. There are at most 16 bindings; keys are 1 to 32 printable characters (no control, invisible or non-space whitespace characters)
/// and compare ignoring case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [
    { "key": "F1", "expression": "happy" },
    { "key": "F3", "expression": "surprised", "mode": "hold" }
]))]
pub struct Bind {
    /// The key, such as `F1`. `F1` and `f1` are the same key.
    pub key: String,
    /// An expression of the character's face, such as `happy`.
    pub expression: String,
    /// `toggle` (each press shows or hides it) or `hold` (shows while the key is down). Default `toggle`.
    #[serde(default = "default_mode")]
    pub mode: TriggerMode,
}

impl Op for Bind {
    fn meta() -> OpMeta {
        OpMeta {
            name: "trigger.bind",
            summary: "Bind a key to a face expression; pressing the key shows it, toggled or held.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_key(&self.key).map_err(op_error)?;
        let expression =
            ExpressionId::new(&self.expression).map_err(|e| OpError::invalid("expression", e.to_string()))?;
        let host = host(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "key": self.key, "expression": self.expression })));
        }
        let expressions = host.expressions();
        if !expressions.0.contains_key(&expression) {
            let names = expressions.0.keys().map(ExpressionId::as_str);
            let hint = match nearest(&self.expression, names) {
                Some(near) => format!("did you mean '{near}'? use an expression from the character's face"),
                None => "use an expression from the character's face".to_owned(),
            };
            return Err(OpError::not_found(format!("no expression '{}' on this character", self.expression))
                .hint(hint));
        }
        let before = binding_of(host, &self.key);
        let binding = TriggerBinding { key: self.key.clone(), expression, mode: self.mode };
        let outcome = host.bind_trigger(binding.clone()).map_err(op_error)?;
        // The binding table is outside the document: the step records the calls that put it back. A new key is undone
        // by unbinding it, a replaced one by binding what it was.
        let undo = match &before {
            Some(prior) => bind_call(prior)?,
            None => unbind_call(&binding.key)?,
        };
        let effect = Effect {
            summary: format!("bound {} to {} ({})", binding.key, binding.expression.as_str(), mode_name(binding.mode)),
            undo,
            redo: bind_call(&binding)?,
        };
        answer_with(outcome, Some(effect))
    }
}

/// Remove a key's trigger binding. An expression it was showing fades out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{ "key": "F1" }]))]
pub struct Unbind {
    /// The key to unbind.
    pub key: String,
}

impl Op for Unbind {
    fn meta() -> OpMeta {
        OpMeta {
            name: "trigger.unbind",
            summary: "Remove a key's trigger binding; the expression it shows fades out.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_key(&self.key).map_err(op_error)?;
        let host = host(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "key": self.key })));
        }
        let before = binding_of(host, &self.key);
        let removed = host.unbind_trigger(&self.key).map_err(op_error)?;
        // Nothing was bound, so nothing changed and there is nothing to undo.
        let effect = match (&before, removed) {
            (Some(prior), true) => Some(Effect {
                summary: format!("unbound {}", prior.key),
                undo: bind_call(prior)?,
                redo: unbind_call(&prior.key)?,
            }),
            _ => None,
        };
        answer_with(json!({ "removed": removed }), effect)
    }
}

/// Press (`down`) or release a bound key, as a keyboard would. A press on a binding whose expression is missing does
/// nothing and says so.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{ "key": "F1" }, { "key": "F3", "down": false }]))]
pub struct Press {
    /// The key to press.
    pub key: String,
    /// `true` for key down, `false` for key up. Default `true`.
    #[serde(default = "default_down")]
    pub down: bool,
}

impl Op for Press {
    fn meta() -> OpMeta {
        OpMeta {
            name: "trigger.press",
            summary: "Press (down) or release a bound key; a binding whose expression is missing does nothing.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_key(&self.key).map_err(op_error)?;
        let host = host(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "key": self.key, "down": self.down })));
        }
        answer(json!({ "pressed": host.press(&self.key, self.down).map_err(op_error)? }))
    }
}

/// The bindings the answer lists.
#[derive(Serialize)]
struct Listing {
    triggers: Vec<TriggerInfo>,
    /// How many bindings there can be.
    max: usize,
}

/// List the key triggers: each key, its expression and mode, whether it is showing and whether its expression is
/// missing from the character.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct List {}

impl Op for List {
    fn meta() -> OpMeta {
        OpMeta {
            name: "trigger.list",
            summary: "List the key triggers: each key, its expression and mode, and whether it is showing or missing.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        answer(Listing { triggers: host(ctx)?.triggers(), max: MAX_TRIGGERS })
    }
}

omuzaki_ops_api::register_op!(Bind);
omuzaki_ops_api::register_op!(Unbind);
omuzaki_ops_api::register_op!(Press);
omuzaki_ops_api::register_op!(List);
