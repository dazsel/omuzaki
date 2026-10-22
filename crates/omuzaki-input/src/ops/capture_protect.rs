//! `input.capture.protect`: set the kill switch's protected programs.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{answer, control};

/// Set the protected programs: while one of them runs, input capture pauses. Names are compared without case and
/// without `.exe`. The answer counts them and never names one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "programs": [] }]))]
pub struct CaptureProtect {
    /// Program names, such as a password manager's. An empty list turns the kill switch off.
    pub programs: Vec<String>,
}

impl Op for CaptureProtect {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.capture.protect",
            summary: "Set the programs that pause input capture while they run.",
            kind: OpKind::Edit,
            scope: Scope::Process,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("the protected programs decide when input capture pauses")?;
        let control = control(ctx)?;
        let programs: Vec<String> =
            self.programs.iter().map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect();
        if !ctx.dry_run {
            control.protect(&programs);
        }
        Ok(Plan::answer(answer(ctx, json!({ "programs": programs.len() }))))
    }
}

register_op!(CaptureProtect);
