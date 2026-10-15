//! `perform.calibrate`: starts a neutral capture ("relax and look at the camera", PF-20).

use omuzaki_anim::MAX_NEUTRAL_FRAMES;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{host, op_error};

/// The fewest frames the op accepts: a second of video at 30 fps, so the median is not one blink's.
pub const MIN_FRAMES: usize = 30;
/// The frames taken when none are given (5 s at 30 fps).
pub const DEFAULT_FRAMES: usize = 150;

fn default_frames() -> usize {
    DEFAULT_FRAMES
}

/// Start a neutral capture: the performer relaxes and looks at the camera, and the median of the tracked frames
/// becomes their rest face. The result applies itself; `perform.status` shows progress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Calibrate {
    /// Tracked frames to take the median over, `30` to `600`. Default `150`.
    #[serde(default = "default_frames")]
    pub frames: usize,
}

impl Op for Calibrate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "perform.calibrate",
            summary: "Start a neutral-face capture so the avatar's rest matches the performer's relaxed face.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(MIN_FRAMES..=MAX_NEUTRAL_FRAMES).contains(&self.frames) {
            return Err(OpError::invalid(
                "frames",
                format!("must be {MIN_FRAMES} to {MAX_NEUTRAL_FRAMES} frames, got {}", self.frames),
            ));
        }
        ctx.require_person("calibration reads the performer's face")?;
        let host = host(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "started": false, "frames": self.frames, "dry_run": true })));
        }
        host.calibrate(self.frames).map_err(op_error)?;
        Ok(Plan::answer(json!({ "started": true, "frames": self.frames })))
    }
}

omuzaki_ops_api::register_op!(Calibrate);
