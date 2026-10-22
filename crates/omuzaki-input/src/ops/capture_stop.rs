//! `input.capture.stop`: stop reading input devices.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{answer, control};

/// Stop input capture. Anyone may, a read-only agent included; stopping when off changes nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct CaptureStop {}

impl Op for CaptureStop {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.capture.stop",
            summary: "Stop input capture; anyone may.",
            kind: OpKind::Edit,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let control = control(ctx)?;
        if !ctx.dry_run {
            control.stop();
        }
        let state = control.report().state.redacted();
        Ok(Plan::answer(answer(ctx, json!({ "state": state }))))
    }
}

register_op!(CaptureStop);
