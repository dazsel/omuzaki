//! `input.capture.state`: what capture is doing and what works here.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::control;

/// The capture state (`off`, `paused`, `on` with the classes read and the degraded ones, or `failed`), each backend's
/// state, what works on this OS, and the detail keys are read at. No device names and no key counts. Why capture paused
/// is shown only to a request that holds the `process` scope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct CaptureStateQuery {}

impl Op for CaptureStateQuery {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.capture.state",
            summary: "Show the input capture state, each backend's state and what works here.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut report = control(ctx)?.report();
        // Every reason is hidden without `process`, so one can't be told from another by elimination (§4).
        if !ctx.allows(Scope::Process) {
            report.state = report.state.redacted();
        }
        Ok(Plan::answer(json!(report)))
    }
}

register_op!(CaptureStateQuery);
