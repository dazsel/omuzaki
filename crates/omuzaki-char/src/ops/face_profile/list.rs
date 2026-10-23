//! `face.list_profiles`: the face-shape presets and their numbers.

use omuzaki_char_api::HeadParams;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::report;
use crate::head::presets::ShapeProfiles;

/// Lists the face-shape presets (CB-10) `face.set_profile` takes: each one's id, what it looks like, its outline and
/// layout, and the silhouette and layout ratios it gives on a default head. Writes nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {}
    ],
    "x-omuzaki-fixture" = "demo"
))]
pub struct FaceListProfiles {}

impl Op for FaceListProfiles {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.list_profiles",
            summary: "List the face-shape presets (round-u, egg-oval, mochi, heart, long): what each looks like, its \
                      outline and layout, and its silhouette and layout ratios.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        let presets = ShapeProfiles::list().map_err(|e| OpError::internal(e.to_string()))?;
        let head = HeadParams::default();
        let listed: Vec<_> = presets
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "summary": p.summary,
                    "profile": p.profile,
                    "measured": report::numbers(&head, &p.profile),
                })
            })
            .collect();
        Ok(Plan::answer(json!({ "presets": listed })))
    }
}

register_op!(FaceListProfiles);
