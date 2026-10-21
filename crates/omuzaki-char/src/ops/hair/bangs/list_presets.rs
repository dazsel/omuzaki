//! `hair.bangs.list_presets`: the bangs presets a fringe can be drawn in.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::presets::PRESETS;

/// The op's name, for the hints of ops that point to it.
pub const NAME: &str = "hair.bangs.list_presets";

/// Lists the bangs presets in their fixed order. Writes nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {}
    ],
    "x-omuzaki-fixture" = "demo"
))]
pub struct HairBangsListPresets {}

impl Op for HairBangsListPresets {
    fn meta() -> OpMeta {
        OpMeta {
            name: NAME,
            summary: "List the bangs presets (blunt, side-swept, curtain, wispy, none) with what each looks like.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        let listed: Vec<_> = PRESETS
            .iter()
            .map(|p| json!({ "id": p.id, "summary": p.summary, "bangs": (p.make)() }))
            .collect();
        Ok(Plan::answer(json!({ "presets": listed })))
    }
}

register_op!(HairBangsListPresets);
