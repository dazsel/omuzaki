//! `input.binding.set_action`: change what a binding does.

use omuzaki_input_api::binding::{BindingAction, BindingId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{answer, check_action, control, error, known};

/// Change what a binding does: run an op, press a trigger slot, or send a locomotion intent. Its keys stay. A key
/// held now still ends what it started; the new action runs from the next press. The action's op must be one this
/// request may run itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingSetAction {
    /// The binding, from `input.binding.list`.
    pub id: BindingId,
    /// `{ "op": { "op": name, "args": {...} } }`, `{ "trigger": slot }` or `{ "intent": ... }`.
    pub action: BindingAction,
}

impl Op for BindingSetAction {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.binding.set_action",
            summary: "Change what an input binding does (an op, a trigger slot or a locomotion intent).",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("a binding runs its action when the person presses its keys")?;
        check_action(ctx, "action", &self.action)?;
        let control = control(ctx)?;
        if ctx.dry_run {
            known(control, self.id)?;
        } else {
            control.set_action(self.id, self.action.clone()).map_err(|e| error(&e))?;
        }
        Ok(Plan::answer(answer(ctx, json!({ "id": self.id, "action": self.action }))))
    }
}

register_op!(BindingSetAction);
