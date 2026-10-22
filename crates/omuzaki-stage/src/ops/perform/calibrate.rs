//! `perform.calibrate`: starts a neutral capture ("relax and look at the camera", PF-20), or puts a calibration back.

use std::collections::BTreeMap;

use omuzaki_anim::MAX_NEUTRAL_FRAMES;
use omuzaki_anim_api::{Calibration, Channel, ChannelRange, DeviceProfile, HeadPose};
use omuzaki_ops_api::{Effect, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
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
///
/// The calibration lives in the performer, not the document, so the step records the calls that put it back: an undo
/// restores the calibration that was in use before (with `restore`), and a redo captures again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Calibrate {
    /// Tracked frames to take the median over, `30` to `600`. Default `150`.
    #[serde(default = "default_frames")]
    pub frames: usize,
    /// A calibration to put back instead of capturing one (what an undo sends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<CalibrationValue>,
}

/// A calibration as an op argument: the device class, each `ARKit` channel's range by name, and the rest head pose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CalibrationValue {
    /// The device class the ranges were made for.
    pub profile: DeviceProfile,
    /// Each `ARKit` channel's range, by its name; a channel left out is `0..1`.
    pub ranges: BTreeMap<String, ChannelRange>,
    /// The performer's relaxed head pose.
    pub head_rest: HeadPose,
}

impl From<&Calibration> for CalibrationValue {
    fn from(calibration: &Calibration) -> Self {
        let ranges = Channel::arkit()
            .iter()
            .map(|info| (info.name.to_owned(), calibration.range(info.channel)))
            .filter(|(_, range)| *range != ChannelRange::IDENTITY)
            .collect();
        Self { profile: calibration.profile, ranges, head_rest: calibration.head_rest }
    }
}

impl CalibrationValue {
    /// The calibration, refusing a channel that isn't an `ARKit` one by name or a range that isn't finite.
    fn calibration(&self) -> Result<Calibration, OpError> {
        let mut calibration = Calibration::identity(self.profile);
        for (name, range) in &self.ranges {
            let field = format!("restore/ranges/{name}");
            let channel = Channel::from_name(name)
                .filter(|c| c.is_arkit())
                .ok_or_else(|| OpError::invalid(&field, format!("'{name}' is not an ARKit channel")))?;
            if !(range.lo.is_finite() && range.hi.is_finite()) {
                return Err(OpError::invalid(&field, "the range is not finite"));
            }
            calibration.set_range(channel, *range);
        }
        calibration.head_rest = self.head_rest;
        Ok(calibration)
    }
}

impl Calibrate {
    /// The call that puts `calibration` back.
    fn restore_call(calibration: &Calibration) -> Result<OpCall, OpError> {
        let args = Self { frames: DEFAULT_FRAMES, restore: Some(calibration.into()) };
        Self::call(&args)
    }

    fn call(args: &Self) -> Result<OpCall, OpError> {
        let args = serde_json::to_value(args).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(OpCall { op: Self::meta().name.to_owned(), args })
    }
}

impl Op for Calibrate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "perform.calibrate",
            summary: "Start a neutral-face capture so the avatar's rest matches the performer's relaxed face (or put \
                      back a calibration with `restore`).",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if let Some(value) = &self.restore {
            let calibration = value.calibration()?;
            let host = host(ctx)?;
            if ctx.dry_run {
                return Ok(Plan::answer(json!({ "restored": false, "dry_run": true })));
            }
            let before = host.requested_calibration();
            host.set_calibration(calibration).map_err(op_error)?;
            let effect = Effect {
                summary: "put a calibration back".to_owned(),
                undo: Self::restore_call(&before)?,
                redo: Self::restore_call(&calibration)?,
            };
            return Ok(Plan { effect: Some(effect), ..Plan::answer(json!({ "restored": true })) });
        }
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
        let before = host.requested_calibration();
        host.calibrate(self.frames).map_err(op_error)?;
        let effect = Effect {
            summary: format!("started a neutral capture of {} frames", self.frames),
            undo: Self::restore_call(&before)?,
            redo: Self::call(&Self { frames: self.frames, restore: None })?,
        };
        Ok(Plan { effect: Some(effect), ..Plan::answer(json!({ "started": true, "frames": self.frames })) })
    }
}

omuzaki_ops_api::register_op!(Calibrate);
