//! `input.binding.list`: the bindings, never their chords.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::control;

/// List the input bindings: each one's id, name, device class, action and whether it shares its keys with another.
/// The keys themselves are shown only in the binding editor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct BindingList {}

impl Op for BindingList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.binding.list",
            summary: "List the input bindings (id, name, device, action, conflict); never their keys.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let control = control(ctx)?;
        Ok(Plan::answer(json!({ "bindings": control.bindings(), "presets": control.presets() })))
    }
}

register_op!(BindingList);
