//! `input.binding.remove`: forget a binding.

use omuzaki_input_api::binding::BindingId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{answer, control, error, known};

/// Remove a binding and its keys. If it is held now, what it started ends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingRemove {
    /// The binding, from `input.binding.list`.
    pub id: BindingId,
}

impl Op for BindingRemove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.binding.remove",
            summary: "Remove an input binding.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("the bindings are the person's")?;
        let control = control(ctx)?;
        if ctx.dry_run {
            known(control, self.id)?;
        } else {
            control.remove(self.id).map_err(|e| error(&e))?;
        }
        Ok(Plan::answer(answer(ctx, json!({ "removed": self.id }))))
    }
}

register_op!(BindingRemove);
