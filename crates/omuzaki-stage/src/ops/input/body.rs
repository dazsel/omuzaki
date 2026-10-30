//! `input.body`: how body tracking is doing (PF-06, PF-07).

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{host, to_value};

/// Read the body tracking: what it was asked to track (`config`), where it stands (`off`, `loading`, `running`, or
/// `failed` with the reason), how sure it is of each arm and hand on the newest frame (left, right; 0 to 1), the
/// frames tracked and what one frame costs (median and 95th percentile, milliseconds).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct Body {}

impl Op for Body {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.body",
            summary: "Read the body tracking: what it tracks, its state, how sure it is of each arm and hand, its cost.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(to_value(host(ctx)?.body())?))
    }
}

omuzaki_ops_api::register_op!(Body);
