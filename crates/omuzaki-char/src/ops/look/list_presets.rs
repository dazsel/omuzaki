//! `look.list_presets`: the look presets every character can be drawn in.

use omuzaki_char_api::{DEFAULT_PRESET, look_presets};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;

/// Lists the registered look presets, sorted by id. Writes nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {}
    ],
    "x-omuzaki-fixture" = "demo"
))]
pub struct LookListPresets {}

impl Op for LookListPresets {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.list_presets",
            summary: "List the look presets (id, name, what it looks like) and which one characters use by default.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        let presets = look_presets().map_err(|e| access::refusal(&e, "preset"))?;
        let listed: Vec<_> =
            presets.iter().map(|p| json!({ "id": p.id, "name": p.name, "summary": p.summary })).collect();
        Ok(Plan::answer(json!({ "default": DEFAULT_PRESET, "presets": listed })))
    }
}

register_op!(LookListPresets);
