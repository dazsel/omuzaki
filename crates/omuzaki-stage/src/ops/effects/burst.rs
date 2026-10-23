//! `stage.effect.burst`: a burst of a preset for the next frame (particles.md D11, D12). A live action: logged and
//! announced, never in undo, never a document edit.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::BurstRequest;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{effect_error, service};
use crate::effects::service::check_burst;

/// Bursts an effect preset (confetti, hit stars, emote rain) on the next frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(extend(
    "examples" = [{"preset": "confetti"}, {"preset": "confetti", "at": {"from": "subject", "point": "chest"}, "count": 200}, {"preset": "hit-stars", "at": {"from": "world", "offset": [0.2, 1.4, 0.3]}}, {"preset": "emote-rain", "emotes": [{"id": "25", "name": "Kappa"}], "only_with_emotes": true}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct EffectBurst(pub BurstRequest);

impl Op for EffectBurst {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.effect.burst",
            summary: "Burst an effect on the next frame: a preset (confetti, hit-stars, emote-rain) from an anchor \
                      (the subject or a member and a bone, the camera, the stage or a world point), with an optional \
                      count, colours, scale, chat emotes and seed. A live action: never part of the document or undo.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let effects = service(ctx)?;
        let (preset, _) = check_burst(&self.0).map_err(effect_error)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "preset": preset.name.as_str() })));
        }
        let accepted = effects.burst(&self.0).map_err(effect_error)?;
        Ok(Plan::answer(json!(accepted)))
    }
}

register_op!(EffectBurst);
