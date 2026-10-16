//! `input.preview`: start or stop the live mirror, the capture the blink check and Calibrate reuse.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{changed, host, mode, op_error};

/// Start (`on: true`) or stop the live mirror: the tracker runs on the bound camera and drives the face. Starting a
/// running preview or stopping a stopped one changes nothing. A preview never outlives the app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{ "on": true }, { "on": false }]))]
pub struct Preview {
    /// `true` to start, `false` to stop.
    pub on: bool,
}

impl Op for Preview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.preview",
            summary: "Start or stop the live camera preview that drives the face; stopping is always allowed.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        // Stopping only takes exposure away, so anyone may; starting a capture is the person's to confirm.
        if self.on {
            ctx.require_person("the preview starts the camera")?;
        }
        let before = host(ctx)?.preview(self.on, mode(ctx)).map_err(op_error)?;
        let answer = json!({ "on": self.on, "before": before, "dry_run": ctx.dry_run });
        let summary = format!("camera preview {}", if self.on { "started" } else { "stopped" });
        changed(Self::meta().name, "on", before, self.on, summary, answer)
    }
}

omuzaki_ops_api::register_op!(Preview);
