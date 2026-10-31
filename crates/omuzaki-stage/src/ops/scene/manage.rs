//! Reading and managing whole scenes (design D1, D8, D13): `stage.describe_scene`, `stage.rename_scene`,
//! `stage.duplicate_scene` and `stage.delete_scene`.

use omuzaki_char_api::paths as char_paths;
use omuzaki_doc_api::{Change, Patch, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{Framing, LightShape, MAX_CAMERAS, MAX_CAST, MAX_LIGHTS, SceneDef, SceneId, paths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::{SceneEdit, refused, scene_id, scene_ids};
use super::put;
use crate::scene::default::opening_scene;

/// Describes a scene: its settings, lights, cameras and cast, and what the validator notices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, {"scene": "night_stream"}], "x-omuzaki-fixture" = "scenes"))]
pub struct DescribeScene {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
}

impl Op for DescribeScene {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.describe_scene",
            summary: "Describe a scene: name, subject, opening camera, backdrop, ground, post, every light, camera and \
                      cast member, and findings such as a cast member whose character is missing.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let doc = ctx.doc;
        let id = scene_id(doc, self.scene.as_ref())?;
        let node = doc.get(&paths::scene(&id)).cloned().unwrap_or(Value::Null);
        let def = SceneDef::from_node(&node)
            .map_err(|e| OpError::invalid("scene", format!("scene '{id}' doesn't read: {e}")))?;
        let has_character = |c: &omuzaki_char_api::CharId| doc.get(&char_paths::character(c)).is_some();
        // A stored scene that breaks a rule is still described: the refusal is a finding here.
        let findings = match def.check_with(&has_character) {
            Ok(found) => serde_json::to_value(found).unwrap_or_default(),
            Err(error) => json!([{ "kind": "invalid", "path": error.path, "message": error.rule }]),
        };
        let lights: Vec<Value> = def
            .lighting
            .lights
            .iter()
            .map(|(light, l)| {
                let shape = match l.shape {
                    LightShape::Directional { azimuth, elevation } => {
                        json!({ "kind": "directional", "azimuth": azimuth, "elevation": elevation })
                    }
                    LightShape::Point { position, range } => {
                        json!({ "kind": "point", "position": position, "range": range })
                    }
                    LightShape::Spot { position, target, cone, penumbra, range } => json!({
                        "kind": "spot", "position": position, "target": target, "cone": cone, "penumbra": penumbra,
                        "range": range
                    }),
                };
                json!({ "id": light.as_str(), "role": l.role, "colour": l.colour, "intensity": l.intensity,
                        "shadow": l.shadow, "anchor": l.anchor.to_string(), "shape": shape })
            })
            .collect();
        let cameras: Vec<Value> = def
            .cameras
            .iter()
            .map(|(camera, c)| {
                let frames = if c.framing == Framing::Free {
                    format!("a free view from {:?} toward {:?}", c.position, c.target)
                } else {
                    format!("{:?} of {}", c.framing, c.subject).to_lowercase()
                };
                json!({ "id": camera.as_str(), "framing": c.framing, "frames": frames, "yaw": c.yaw, "pitch": c.pitch,
                        "opening": def.scene.camera.as_ref() == Some(camera) })
            })
            .collect();
        let cast: Vec<Value> = def
            .cast
            .iter()
            .map(|(member, m)| {
                json!({ "member": member.as_str(), "character": m.character.as_str(), "variant": m.variant,
                        "position": m.position, "yaw": m.yaw, "visible": m.visible,
                        "subject": def.scene.subject.as_ref() == Some(member) })
            })
            .collect();
        let s = &def.scene;
        Ok(Plan::answer(json!({
            "scene": id.as_str(),
            "name": s.name,
            "default": opening_scene(doc).as_ref() == Some(&id),
            "subject": s.subject,
            "camera": s.camera,
            "backdrop": s.backdrop,
            "ground": s.ground,
            "post": s.post,
            "lighting": {
                "rig": def.lighting.rig,
                "exposure": def.lighting.exposure,
                "background": def.lighting.background,
                "outline_light": def.lighting.outline_light,
                "back_light_rim": def.lighting.back_light_rim,
                "shadow_softness": def.lighting.shadow_softness,
                "skin_floor": def.lighting.skin_floor,
                "ambient": def.lighting.ambient,
                "bloom": def.lighting.bloom,
                "fog": def.lighting.fog,
                "environment": def.lighting.environment,
                "lights": lights,
                "limit": format!("{} of {} lights besides the ambient", def.lighting.lights.len(), MAX_LIGHTS - 1),
            },
            "cameras": cameras,
            "cameras_limit": MAX_CAMERAS,
            "cast": cast,
            "cast_limit": MAX_CAST,
            "props": def.props.len(),
            "findings": findings,
        })))
    }
}

register_op!(DescribeScene);

/// Renames a scene: its display name only, the id stays.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"scene": "main", "name": "Morning show"}], "x-omuzaki-fixture" = "scenes"))]
pub struct RenameScene {
    /// The scene.
    pub scene: SceneId,
    /// Its new name.
    pub name: String,
}

impl Op for RenameScene {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.rename_scene",
            summary: "Rename a scene (its display name; the id, which shows and rules refer to, stays).",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(OpError::invalid("name", "a scene's name can't be empty").hint("give it a name people read"));
        }
        let mut edit = SceneEdit::open(ctx.doc, Some(&self.scene))?;
        name.clone_into(&mut edit.def.scene.name);
        let output = json!({ "scene": self.scene.as_str(), "name": name });
        edit.finish(ctx, output)
    }
}

register_op!(RenameScene);

/// Copies a scene under a new id, without the library's records of what was applied to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"scene": "main", "id": "main_copy"}, {"scene": "main", "id": "rehearsal", "name": "Rehearsal"}],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct DuplicateScene {
    /// The scene to copy.
    pub scene: SceneId,
    /// The copy's id.
    pub id: SceneId,
    /// The copy's name; the original's with " (copy)" when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Op for DuplicateScene {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.duplicate_scene",
            summary: "Copy a scene under a new id: lighting, cameras, cast and props (not the library's applied records).",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let doc = ctx.doc;
        let source = scene_id(doc, Some(&self.scene))?;
        if doc.get(&paths::scene(&self.id)).is_some() {
            return Err(OpError::conflict(format!("there is already a scene '{}'", self.id))
                .hint("pick another id for the copy"));
        }
        let node = doc.get(&paths::scene(&source)).cloned().unwrap_or(Value::Null);
        let mut def = SceneDef::from_node(&node)
            .map_err(|e| OpError::invalid("scene", format!("scene '{source}' doesn't read: {e}")))?;
        def.scene.name = self.name.clone().unwrap_or_else(|| format!("{} (copy)", def.scene.name));
        def.check().map_err(refused)?;
        // A fresh node: the copy carries the definition only, so the library's `applied` records stay with the original.
        let mut copy = Value::Null;
        def.to_node(&mut copy).map_err(|e| OpError::internal(format!("writing the copy: {e}")))?;
        let patch = put(doc, &paths::scene(&self.id), copy);
        Ok(Plan {
            patch,
            output: Some(json!({ "scene": self.id.as_str(), "name": def.scene.name })),
            ..Plan::default()
        })
    }
}

register_op!(DuplicateScene);

/// Deletes a scene. If it was the one the project opens in, the first remaining scene becomes the default in the same
/// step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"scene": "night_stream"}, {"scene": "main"}], "x-omuzaki-fixture" = "scenes"))]
pub struct DeleteScene {
    /// The scene.
    pub scene: SceneId,
}

impl Op for DeleteScene {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.delete_scene",
            summary: "Delete a scene; when it was the default, the first remaining scene becomes the default (or none).",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let doc = ctx.doc;
        let id = scene_id(doc, Some(&self.scene))?;
        let at = paths::scene(&id);
        let mut patch = Patch::new().with(at.clone(), doc.get(&at).cloned(), None);
        let default_path = paths::default_scene();
        let stored_default = doc.get(&default_path).and_then(Value::as_str);
        let mut default = stored_default.map(str::to_owned);
        if stored_default == Some(id.as_str()) {
            let next = scene_ids(doc).into_iter().find(|s| s != id.as_str());
            match &next {
                Some(next) => patch.changes.push(Change {
                    path: default_path.clone(),
                    before: doc.get(&default_path).cloned(),
                    after: Some(json!(next)),
                }),
                None => patch.changes.push(Change {
                    path: default_path.clone(),
                    before: doc.get(&default_path).cloned(),
                    after: None,
                }),
            }
            default = next;
        }
        Ok(Plan { patch, output: Some(json!({ "deleted": id.as_str(), "default": default })), ..Plan::default() })
    }
}

register_op!(DeleteScene);
