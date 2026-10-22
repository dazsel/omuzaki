//! The `stage.*` effect ops (particles.md D11): the scene's weather, which is undoable scene state, and the presets.
//! The live actions (bursts, the live weather, clear and status) reach the stage through the
//! [`EffectService`](omuzaki_stage_api::EffectService).

pub mod burst;
pub mod clear;
pub mod list;
pub mod set_weather;
pub mod status;
pub mod weather;

use omuzaki_ops_api::{ErrorCode, OpCtx, OpError};
use omuzaki_stage_api::{EffectError, EffectService};

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
        EffectError::Limited => OpError {
            code: ErrorCode::Limited,
            message: "too many effects are waiting; try again in a moment".into(),
            field: None,
            hint: Some("the effect queue holds 64 requests; it drains every frame".into()),
            detail: None,
        },
        EffectError::Invalid(why) => OpError::invalid("preset", why),
        EffectError::Unsupported => OpError::unsupported("the renderer draws no particles"),
    }
}

/// The effect service the app registered.
///
/// # Errors
/// `Unavailable` when the app runs no stage effects.
pub(crate) fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn EffectService, OpError> {
    ctx.service::<dyn EffectService>()
}
