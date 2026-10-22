//! `stage.set_default_scene`: the scene the project opens in (design D1, D8).

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{SceneId, paths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::put;

/// Makes a scene the one the project opens in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetDefaultScene {
    /// The scene.
    pub scene: SceneId,
}

impl Op for SetDefaultScene {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_default_scene",
            summary: "Make a scene the one the project opens in.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&paths::scene(&self.scene)).is_none() {
            return Err(OpError::not_found(format!("there is no scene '{}'", self.scene))
                .hint("stage.list_scenes lists the project's scenes"));
        }
        let value = json!(self.scene.as_str());
        let path = paths::default_scene();
        let patch = if ctx.doc.get(&path) == Some(&value) { omuzaki_doc_api::Patch::new() } else { put(ctx.doc, &path, value) };
        Ok(Plan { patch, output: Some(json!({ "default": self.scene.as_str() })), ..Plan::default() })
    }
}

register_op!(SetDefaultScene);
