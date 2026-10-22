//! `input.capture.pause`: step capture aside, keeping the classes chosen.

use omuzaki_input_api::capture::CaptureState;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{answer, control};

/// Pause input capture, keeping the classes chosen: for a password prompt or a private call. Anyone may, and it stays
/// paused until a person starts it again. Pausing when paused or off changes nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct CapturePause {}

impl Op for CapturePause {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.capture.pause",
            summary: "Pause input capture, keeping its classes; anyone may, and only a person resumes it.",
            kind: OpKind::Edit,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let control = control(ctx)?;
        if !ctx.dry_run {
            control.pause();
        }
        let state = control.report().state.redacted();
        let paused = !ctx.dry_run || !matches!(state, CaptureState::Off);
        Ok(Plan::answer(answer(ctx, json!({ "paused": paused, "state": state }))))
    }
}

register_op!(CapturePause);
