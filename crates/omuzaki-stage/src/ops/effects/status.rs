//! `stage.effect.status`: what the effects are doing (particles.md D11).

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::service;

/// Reports the effects' state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}], "x-omuzaki-fixture" = "scenes"))]
pub struct EffectStatusOp {}

impl Op for EffectStatusOp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.effect.status",
            summary: "Report the particle effects: live slots and the cap, where the weather comes from (scene, \
                      override or none), the density the frame-budget scheduler set, requests waiting for the next \
                      frame, particles thinned, whether the renderer draws particles, and what looked wrong.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!(service(ctx)?.status())))
    }
}

register_op!(EffectStatusOp);
