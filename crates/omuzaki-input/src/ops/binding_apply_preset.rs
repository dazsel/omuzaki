//! `input.binding.apply_preset`: add a preset's bindings.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{answer, check_action, control, error};

/// Apply a preset (`locomotion`: W A S D to walk, Space to jump, left Shift to run, C to sit). A binding with the same
/// name takes the preset's keys and action; the others are added. Nothing is bound until a preset is applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "preset": "locomotion" }]))]
pub struct BindingApplyPreset {
    /// The preset's name.
    pub preset: String,
}

impl Op for BindingApplyPreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.binding.apply_preset",
            summary: "Apply an input binding preset (locomotion).",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("a preset binds keys the person presses")?;
        let control = control(ctx)?;
        for action in control.preset_actions(&self.preset).map_err(|e| error(&e))? {
            check_action(ctx, "preset", &action)?;
        }
        let ids = if ctx.dry_run { Vec::new() } else { control.apply_preset(&self.preset).map_err(|e| error(&e))? };
        Ok(Plan::answer(answer(ctx, json!({ "preset": self.preset, "bindings": ids }))))
    }
}

register_op!(BindingApplyPreset);
