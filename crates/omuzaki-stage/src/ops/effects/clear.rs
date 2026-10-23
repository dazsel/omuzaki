//! `stage.effect.clear`: every live particle shrinks away and the live weather override ends (particles.md D11): the
//! panic button's op.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{effect_error, service};
use crate::effects::service::check_clear;

/// The default fade, in seconds.
const FADE_S: f32 = 0.3;

/// Clears every particle effect.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, {"fade_s": 1.5}, {"fade_s": 0}], "x-omuzaki-fixture" = "scenes"))]
pub struct EffectClear {
    /// Seconds over which the particles shrink away, `0..=5`; 0.3 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_s: Option<f32>,
}

impl Op for EffectClear {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.effect.clear",
            summary: "Clear every particle effect: live particles shrink away over `fade_s` seconds (0.3 by default) \
                      and the live weather override ends. The scene's own weather starts again.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let effects = service(ctx)?;
        let fade_s = self.fade_s.unwrap_or(FADE_S);
        check_clear(fade_s).map_err(|e| OpError::invalid("fade_s", e.to_string()))?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "fade_s": fade_s })));
        }
        effects.clear(fade_s).map_err(effect_error)?;
        Ok(Plan::answer(json!({ "fade_s": fade_s })))
    }
}

register_op!(EffectClear);
