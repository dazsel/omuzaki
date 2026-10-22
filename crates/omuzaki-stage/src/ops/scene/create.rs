//! `stage.create_scene`: a new scene with the default content (design D8, D11).

use omuzaki_char_api::{CharId, paths as char_paths};
use omuzaki_core::text::nearest;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{CastMember, MemberId, SceneId, paths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::put;
use crate::scene::default::default_scene;

/// Creates a scene with the default look: the studio rig, a shadow ground, the full, bust and face cameras.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateScene {
    /// The new scene's id, a slug like `main` or `night_stream`.
    pub id: SceneId,
    /// Its name; the id with a capital first letter when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The characters to cast, each as a member named after it; the project's default character when left out, and
    /// nobody when given empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast: Option<Vec<CharId>>,
}

/// `id` with its first letter in capitals and underscores as spaces: `night_stream` is "Night stream".
fn name_of(id: &SceneId) -> String {
    let spaced = id.as_str().replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl Op for CreateScene {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.create_scene",
            summary: "Create a scene with the default look (studio light rig, shadow ground, full, bust and face \
                      cameras) and a cast; the first scene of a project becomes its default.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let doc = ctx.doc;
        let path = paths::scene(&self.id);
        if doc.get(&path).is_some() {
            return Err(OpError::conflict(format!("there is already a scene '{}'", self.id))
                .hint("pick another id, or edit that scene"));
        }
        let mut scene = default_scene(doc)?;
        scene.scene.name = self.name.clone().unwrap_or_else(|| name_of(&self.id));
        if let Some(cast) = &self.cast {
            scene.cast.clear();
            scene.scene.subject = None;
            let known: Vec<String> = doc
                .get(&DocPath::root().join("characters"))
                .and_then(Value::as_object)
                .map(|c| c.keys().cloned().collect())
                .unwrap_or_default();
            for character in cast {
                if doc.get(&char_paths::character(character)).is_none() {
                    let mut error = OpError::invalid("cast", format!("there is no character '{character}'"));
                    if let Some(near) = nearest(character.as_str(), known.iter().map(String::as_str)) {
                        error = error.hint(format!("did you mean '{near}'?"));
                    }
                    return Err(error);
                }
                let member = MemberId::new(character.as_str())
                    .map_err(|e| OpError::invalid("cast", format!("'{character}' can't name a member: {e}")))?;
                scene.scene.subject.get_or_insert_with(|| member.clone());
                scene.cast.insert(member, CastMember::new(character.clone()));
            }
        }
        let has_character = |id: &CharId| doc.get(&char_paths::character(id)).is_some();
        scene.check_with(&has_character).map_err(|e| OpError::invalid("scene", e.to_string()))?;
        let mut node = Value::Null;
        scene.to_node(&mut node).map_err(|e| OpError::internal(format!("writing the scene: {e}")))?;
        let mut patch = put(doc, &path, node);
        let becomes_default = doc.get(&paths::default_scene()).is_none();
        if becomes_default {
            patch.extend(put(doc, &paths::default_scene(), json!(self.id.as_str())));
        }
        let output = json!({ "scene": self.id.as_str(), "name": scene.scene.name, "default": becomes_default });
        Ok(Plan { patch, output: Some(output), ..Plan::default() })
    }
}

register_op!(CreateScene);
