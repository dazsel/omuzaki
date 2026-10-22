//! `render.golden.set_scene`: adds, changes or removes one shot of a character's golden manifest without rendering, so
//! a tolerance can be loosened or a shot added before the next bless. A change to what the shot renders (anything but
//! its tolerance) drops that shot's goldens, which the next bless records again.

use omuzaki_char_api::{CharId, paths};
use omuzaki_core::id::Slug;
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::manifest::{GoldenManifest, GoldenScene, MAX_SCENES, manifest_path};
use crate::ops::scene::put;

/// Sets or removes one shot of a character's golden manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "scene": "face", "shot": {"framing": "face", "size": [256, 256], "tolerance": {"per_channel": 4, "max_changed": 0.002}}},
        {"character": "kit", "scene": "wave", "shot": {"pose": {"id": "wave-right"}}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct GoldenSetScene {
    /// The character.
    pub character: CharId,
    /// The shot's name in the manifest.
    pub scene: String,
    /// The shot; leave out to remove it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot: Option<GoldenScene>,
}

impl Op for GoldenSetScene {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.golden.set_scene",
            summary: "Add, change or remove (no `shot`) one shot of a character's golden manifest, without rendering. A \
                      new tolerance keeps the shot's goldens; any other change drops them until render.golden.bless \
                      records them again.",
            kind: OpKind::Edit,
            scope: Scope::Render,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        Slug::new(self.scene.as_str())
            .map_err(|e| OpError::invalid("scene", format!("'{}' is not a scene name: {e}", self.scene)))?;
        // A new manifest starts from the default shots, as a first bless would.
        let mut manifest = GoldenManifest::read(ctx.doc, &self.character)?
            .unwrap_or_else(|| GoldenManifest { scenes: GoldenScene::defaults(), ..GoldenManifest::default() });
        let old = manifest.scenes.get(&self.scene).cloned();
        if let Some(shot) = &self.shot {
            shot.check("shot")?;
            if old.is_none() && manifest.scenes.len() >= MAX_SCENES {
                return Err(OpError::invalid("scene", format!("the manifest has {MAX_SCENES} shots already")));
            }
            let renders_alike = old.as_ref().is_some_and(|old| {
                GoldenScene { tolerance: shot.tolerance, ..old.clone() } == *shot
            });
            if !renders_alike {
                for golden in manifest.variants.values_mut() {
                    golden.images.remove(&self.scene);
                }
            }
            manifest.scenes.insert(self.scene.clone(), shot.clone());
        } else {
            if old.is_none() {
                return Err(OpError::not_found(format!("the manifest has no shot '{}'", self.scene)));
            }
            if manifest.scenes.len() == 1 {
                return Err(OpError::invalid("scene", "a manifest keeps at least one shot"));
            }
            manifest.scenes.remove(&self.scene);
            for golden in manifest.variants.values_mut() {
                golden.images.remove(&self.scene);
            }
        }
        manifest.check()?;
        let value = serde_json::to_value(&manifest).map_err(|e| OpError::internal(format!("the manifest: {e}")))?;
        let at = manifest_path(&self.character);
        let patch = if ctx.doc.get(&at) == Some(&value) { Patch::new() } else { put(ctx.doc, &at, value) };
        Ok(Plan { patch, output: Some(json!({ "scenes": manifest.scenes.keys().collect::<Vec<_>>() })), ..Plan::default() })
    }
}

register_op!(GoldenSetScene);
