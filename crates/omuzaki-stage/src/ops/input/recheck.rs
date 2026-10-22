//! `input.recheck`: the tracking doctor forgets what it measured and measures again, after a fix (PF-28).

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::doctor::finding;
use super::host;

/// Ask the tracking doctor to measure again from now: after turning on a lamp or moving the camera, the findings clear
/// and come back only if the problem is still there (within about a second). Answers the findings it cleared. It
/// changes nothing the app keeps, so it carries no undo call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct Recheck {}

impl Op for Recheck {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.recheck",
            summary: "Make the tracking doctor measure again from now, after a fix; answers what it cleared.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = host(ctx)?;
        let cleared: Vec<Value> = host.diagnoses().iter().map(finding).collect();
        if !ctx.dry_run {
            host.recheck();
        }
        Ok(Plan::answer(json!({ "cleared": cleared, "dry_run": ctx.dry_run })))
    }
}

omuzaki_ops_api::register_op!(Recheck);
