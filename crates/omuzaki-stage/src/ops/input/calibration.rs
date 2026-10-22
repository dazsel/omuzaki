//! `input.calibration`: where the guided calibration stands (PF-20).

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{host, to_value};

/// Read the guided calibration: `idle`, `waiting` for the first frame, `running` with the prompt showing and the
/// seconds left in it and in the routine, `done` with the prompts it could not measure, `failed` with why, or
/// `cancelled`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct Calibration {}

impl Op for Calibration {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.calibration",
            summary: "Read the guided calibration: the prompt showing, the time left, or how it ended.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(to_value(host(ctx)?.guided())?))
    }
}

omuzaki_ops_api::register_op!(Calibration);
