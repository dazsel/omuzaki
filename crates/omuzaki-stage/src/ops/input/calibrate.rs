//! `input.calibrate`: the guided calibration (PF-20), about forty seconds of prompts over the running capture.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{host, mode, op_error, to_value};

/// Start the guided calibration, or stop it with `cancel: true`. The person follows about forty seconds of prompts
/// (relax, big smile, brows up and down, eyes wide, squint, mouth open, kiss, cheeks puffed, one-sided smiles, eyes
/// closed, look around) while the camera preview runs; each control's range is then fitted to their face and used
/// at once, and kept with the character for this camera. `input.calibration` shows the prompt and the time left.
/// Stopping keeps the earlier calibration. A finished calibration is replaced by calibrating again, so this carries no
/// undo call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}, { "cancel": true }]))]
pub struct Calibrate {
    /// `true` stops a running calibration instead of starting one.
    #[serde(default)]
    pub cancel: bool,
}

impl Op for Calibrate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.calibrate",
            summary: "Start the guided face calibration (about 40 s of prompts) over the running preview, or cancel it.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = host(ctx)?;
        if self.cancel {
            // Stopping only takes the routine away, so anyone may.
            let running = host.guided().is_running();
            if !ctx.dry_run {
                host.cancel_guided();
            }
            return Ok(Plan::answer(json!({ "cancelled": running, "dry_run": ctx.dry_run })));
        }
        ctx.require_person("calibration reads the performer's face")?;
        let status = host.calibrate_guided(mode(ctx)).map_err(op_error)?;
        Ok(Plan::answer(json!({ "started": !ctx.dry_run, "status": to_value(status)?, "dry_run": ctx.dry_run })))
    }
}

omuzaki_ops_api::register_op!(Calibrate);
