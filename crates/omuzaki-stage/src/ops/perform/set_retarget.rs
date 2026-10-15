//! `perform.set_retarget`: the smoothing, exaggeration and head-gain dials and the mirror switch (PF-21).

use std::ops::RangeInclusive;

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{host, op_error};
use crate::perform::host::RetargetChange;

/// Allowed smoothing, raw to very smooth.
pub const SMOOTHING: RangeInclusive<f32> = 0.0..=1.0;
/// Allowed exaggeration, literal to strong.
pub const EXAGGERATION: RangeInclusive<f32> = 1.0..=3.0;
/// Allowed head gain.
pub const HEAD_GAIN: RangeInclusive<f32> = 0.0..=3.0;

/// Change the retarget's settings; fields left out stay as they are. The change is queued, not waited for (the op
/// runs under the document lock): it returns the settings as requested and a `seq`, and `perform.status` shows the
/// settings the performer is using, with `retarget_seq` reaching `seq` once it has applied this change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SetRetarget {
    /// Smoothing, `0` raw to `1` very smooth.
    #[serde(default)]
    pub smoothing: Option<f32>,
    /// Exaggeration, `1` literal to `3`; `1.5` reads as anime.
    #[serde(default)]
    pub exaggeration: Option<f32>,
    /// Multiplier on head angles, `0` to `3`.
    #[serde(default)]
    pub head_gain: Option<f32>,
    /// Swap left and right, for sources whose preview is a selfie mirror.
    #[serde(default)]
    pub mirror: Option<bool>,
}

/// Rejects `value` outside `range`, naming `field`; never clamps. NaN is outside every range.
fn check(field: &str, value: Option<f32>, range: &RangeInclusive<f32>) -> Result<(), OpError> {
    match value {
        Some(v) if !range.contains(&v) => {
            Err(OpError::invalid(field, format!("must be {} to {}, got {v}", range.start(), range.end())))
        }
        _ => Ok(()),
    }
}

impl Op for SetRetarget {
    fn meta() -> OpMeta {
        OpMeta {
            name: "perform.set_retarget",
            summary: "Queue a change to the live face retarget (smoothing, exaggeration, head gain, mirror); returns the requested settings and a seq, and perform.status shows when it is applied.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check("smoothing", self.smoothing, &SMOOTHING)?;
        check("exaggeration", self.exaggeration, &EXAGGERATION)?;
        check("head_gain", self.head_gain, &HEAD_GAIN)?;
        let host = host(ctx)?;
        let change = RetargetChange {
            smoothing: self.smoothing,
            exaggeration: self.exaggeration,
            head_gain: self.head_gain,
            mirror: self.mirror,
        };
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "change": change })));
        }
        let receipt = host.set_retarget(change).map_err(op_error)?;
        let receipt = serde_json::to_value(receipt).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan::answer(receipt))
    }
}

omuzaki_ops_api::register_op!(SetRetarget);
