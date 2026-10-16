//! `input.request_camera`: show the OS camera permission prompt. The prompt is the person's alone.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{host, person_only};

/// Ask the OS for camera access (its own dialog, where it has one) and answer `granted`, `denied` or `unknown`. Only
/// the person can do this, in the Omuzaki window. It changes nothing the app can take back (only the person can, in
/// the OS settings), so it carries no undo call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct RequestCamera {}

impl Op for RequestCamera {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.request_camera",
            summary: "Show the OS camera permission prompt (person only) and answer the permission.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        person_only(ctx, "the OS camera dialog is the person's to answer")?;
        let host = host(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "permission": host.state().permission })));
        }
        Ok(Plan::answer(json!({ "permission": host.request_camera() })))
    }
}

omuzaki_ops_api::register_op!(RequestCamera);
