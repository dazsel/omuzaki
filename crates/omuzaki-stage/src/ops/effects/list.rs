//! `stage.effect.list`: the effect presets (particles.md D11).

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::EffectKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::effects::presets;

/// Lists the effect presets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, {"kind": "weather"}], "x-omuzaki-fixture" = "scenes"))]
pub struct ListEffects {
    /// Only `burst` or only `weather` presets; all when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EffectKind>,
}

impl Op for ListEffects {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.effect.list",
            summary: "List the effect presets (confetti, rain, snow...): name, title, summary, kind (burst or \
                      weather) and a burst's default count.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let rows: Vec<_> = presets::infos().iter().filter(|p| self.kind.is_none_or(|k| p.kind == k)).collect();
        Ok(Plan::answer(json!({ "presets": rows })))
    }
}

register_op!(ListEffects);
