//! `input.state`: what the tracking input is doing, and the cameras there are.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{host, to_value};

/// Read the tracking input: the source kind, the bound camera and whether it is plugged in, the preview, the camera
/// permission, the cameras there are (listing them opens none) and whether a face is tracked.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct State {}

impl Op for State {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.state",
            summary: "Read the tracking input: source, bound camera, preview, permission, cameras and tracking.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(to_value(host(ctx)?.state())?))
    }
}

omuzaki_ops_api::register_op!(State);
