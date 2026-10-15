//! `perform.status`: what the performer is doing right now.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::host;

/// Read the performer's status: whether a face is tracked, the calibration capture, the retarget settings and the
/// tracker's frame rate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Status {}

impl Op for Status {
    fn meta() -> OpMeta {
        OpMeta {
            name: "perform.status",
            summary: "Read the performer's tracking state, calibration progress, retarget settings and frame rate.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let status = host(ctx)?.status();
        let status = serde_json::to_value(status).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan::answer(status))
    }
}

omuzaki_ops_api::register_op!(Status);
