//! `stage.effect.weather`: the live weather override (particles.md D10, D11): a weather preset for a while, `none`,
//! or `scene` to give the scene's weather back. A live action: the scene's own weather is left as it is.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::LiveWeather;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{effect_error, service};
use crate::effects::service::check_weather;

/// Sets the live weather: a preset (for `for_s` seconds or until changed), `none` or `scene`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(extend(
    "examples" = [{"preset": "snow"}, {"preset": "rain", "intensity": 0.8, "for_s": 60}, {"preset": "none"}, {"preset": "scene"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct EffectWeather(pub LiveWeather);

impl Op for EffectWeather {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.effect.weather",
            summary: "Change the live weather: a weather preset (rain, snow, petals) at an intensity, for a while or \
                      until changed; `none` for no weather; `scene` for the scene's own again. A live action: the \
                      scene's saved weather (stage.set_weather) is not changed.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let effects = service(ctx)?;
        check_weather(&self.0).map_err(effect_error)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false })));
        }
        effects.live_weather(&self.0).map_err(effect_error)?;
        Ok(Plan::answer(json!({})))
    }
}

register_op!(EffectWeather);
