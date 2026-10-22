//! The `stage.*` effect ops (particles.md D11): the scene's weather, which is undoable scene state, and the presets.
//! The live actions (bursts, the live weather, clear and status) reach the stage through the
//! [`EffectService`](omuzaki_stage_api::EffectService).

pub mod list;
pub mod set_weather;

use omuzaki_ops_api::OpError;
use omuzaki_stage_api::EffectError;

/// The service's refusal as an op error: an unknown preset names the nearest ones and `stage.effect.list`.
pub(crate) fn effect_error(error: EffectError) -> OpError {
    match error {
        EffectError::UnknownPreset { name, nearest, .. } => {
            let mut refused = OpError::not_found(format!("there is no effect preset '{name}'"));
            refused.field = Some("preset".into());
            match nearest.as_slice() {
                [] => refused.hint("stage.effect.list lists the presets"),
                near => refused.hint(format!("did you mean '{}'? stage.effect.list lists the presets", near.join("', '"))),
            }
        }
        EffectError::Limited => OpError::conflict("too many effects are waiting; try again in a moment"),
        EffectError::Invalid(why) => OpError::invalid("preset", why),
        EffectError::Unsupported => OpError::unsupported("the renderer draws no particles"),
    }
}
