//! `input.health`: how the running capture is doing (PF-26), the numbers the health meter shows.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{host, to_value};

/// Read the tracking health: whether a capture runs, its frame rate, its latency (median, 95th percentile and worst),
/// the frames lost, its use of the 5 ms face-tracking budget (webcam only) and what the tracking doctor finds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct Health {}

impl Op for Health {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.health",
            summary: "Read the tracking health: frame rate, latency, lost frames, budget use and the doctor's findings.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(to_value(host(ctx)?.health())?))
    }
}

omuzaki_ops_api::register_op!(Health);
