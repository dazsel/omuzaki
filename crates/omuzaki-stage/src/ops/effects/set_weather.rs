//! `stage.set_weather`: the scene's weather (particles.md D10, D11): a weather preset's values copied into the scene,
//! so retuning the preset never changes a saved scene, or `none` to remove it. One patch, undoable.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{EffectKind, SceneId, WeatherArea};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::effect_error;
use crate::effects::presets;
use crate::ops::scene::edit::SceneEdit;

/// The `preset` that removes the weather.
const NONE: &str = "none";

/// Sets or removes a scene's weather.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"preset": "snow"}, {"scene": "night_stream", "preset": "rain", "intensity": 0.6}, {"preset": "petals", "area": {"anchor": "stage", "min": [-2, 0, -2], "max": [2, 0, 1]}}, {"preset": "none"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetWeather {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// A weather preset (`rain`, `snow`, `petals`; `stage.effect.list {kind: "weather"}` lists them), or `none` to
    /// remove the weather.
    pub preset: String,
    /// How much of the preset's density, `0..=1`; 1 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    /// Where it falls: `auto` (around the cast and the active camera, the default) or `{anchor, min, max}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<WeatherArea>,
}

impl Op for SetWeather {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_weather",
            summary: "Set a scene's weather from a preset (rain, snow, petals) at an intensity, or `none` to remove \
                      it. The preset's values are copied into the scene, where any of them can be edited.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        if self.preset == NONE {
            if let Some(field) = [("intensity", self.intensity.is_some()), ("area", self.area.is_some())]
                .into_iter()
                .find_map(|(field, set)| set.then_some(field))
            {
                return Err(OpError::invalid(field, format!("`none` removes the weather and takes no `{field}`")));
            }
            edit.def.weather = None;
            let output = json!({ "scene": edit.id.as_str(), "weather": null });
            return edit.finish(ctx, output);
        }
        let intensity = self.intensity.unwrap_or(1.0);
        if !(intensity.is_finite() && (0.0..=1.0).contains(&intensity)) {
            return Err(OpError::invalid("intensity", format!("must be between 0 and 1, got {intensity}"))
                .hint("1 is the preset's full density; 0 keeps the weather with nothing falling"));
        }
        let preset = presets::find_kind(&self.preset, EffectKind::Weather).map_err(effect_error)?;
        let Some(mut weather) = preset.weather_def(intensity) else {
            return Err(OpError::internal(format!("weather preset '{}' has no weather block", preset.name)));
        };
        if let Some(area) = &self.area {
            weather.area = area.clone();
        }
        edit.def.weather = Some(weather);
        let output = json!({ "scene": edit.id.as_str(), "weather": preset.name.as_str(), "intensity": intensity });
        edit.finish(ctx, output)
    }
}

register_op!(SetWeather);
