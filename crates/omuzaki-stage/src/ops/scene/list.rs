//! `stage.list_scenes`: the project's scenes, which one it opens in, and who is cast in each (design D8).

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{SceneDef, SceneId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::scene::default::opening_scene;

/// Lists the project's scenes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListScenes {}

impl Op for ListScenes {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.list_scenes",
            summary: "List the project's scenes: id, name, cast, and which one the project opens in.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let doc = ctx.doc;
        let opening = opening_scene(doc);
        let mut scenes = Vec::new();
        if let Some(table) = doc.get(&DocPath::root().join("scenes")).and_then(Value::as_object) {
            for (id, node) in table {
                let def = SceneDef::from_node(node)
                    .map_err(|e| OpError::invalid("scene", format!("scene '{id}' doesn't read: {e}")))?;
                let cast: Vec<Value> = def
                    .cast
                    .iter()
                    .map(|(member, m)| json!({ "member": member.as_str(), "character": m.character.as_str() }))
                    .collect();
                let default = opening.as_ref().map(SceneId::as_str) == Some(id.as_str());
                scenes.push(json!({ "id": id, "name": def.scene.name, "cast": cast, "default": default }));
            }
        }
        Ok(Plan::answer(json!({ "scenes": scenes })))
    }
}

register_op!(ListScenes);
