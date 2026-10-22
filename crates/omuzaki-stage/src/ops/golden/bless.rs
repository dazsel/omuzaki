//! `render.golden.bless`: renders a character's variants in the manifest's shots through `render.preview` and records
//! what they look like, as the goldens `render.golden.check` holds later renders to (spec VF-03).

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, VariantId, paths};
use omuzaki_doc_api::DocRead;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::manifest::{
    BASE, GoldenManifest, GoldenScene, GoldenVariant, definition, is_game_face, manifest_path, variants,
};
use crate::ops::render::job::{RenderQuality, cost};
use crate::ops::render::preview::{Preview, PreviewResult};
use crate::ops::scene::put;

/// Renders a character's variants in its golden shots and records them as the goldens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "variants": ["close-up"]},
        {"character": "kit", "scenes": {"face": {"framing": "face", "size": [256, 256]}, "wave": {"pose": {"id": "wave-right"}}}}
    ],
    "x-omuzaki-fixture" = "character-variant"
))]
pub struct GoldenBless {
    /// The character.
    pub character: CharId,
    /// The variants to bless, by id, with `base` for the character's own definition; every variant and the base when
    /// left out. The others keep their goldens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,
    /// The shots to check in, by name, in place of the manifest's (a face and a full shot for a new manifest). New
    /// shots need every golden again, so this blesses every variant and the base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenes: Option<BTreeMap<String, GoldenScene>>,
}

/// What a bless recorded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GoldenBlessResult {
    /// The manifest as it now stands.
    pub manifest: GoldenManifest,
    /// The variants blessed, `base` for the character's own definition.
    pub blessed: Vec<String>,
    /// How many pictures were rendered.
    pub renders: u32,
}

/// One variant to bless, by its manifest name.
struct Target {
    name: String,
    variant: Option<VariantId>,
    pixel_exact: bool,
}

impl GoldenBless {
    /// The variants to bless and the scenes, checked against the document.
    fn prepare(&self, doc: &dyn DocRead) -> Result<(Vec<Target>, GoldenManifest), OpError> {
        if doc.get(&paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        let existing = GoldenManifest::read(doc, &self.character)?;
        let mut manifest = existing.unwrap_or_default();
        if let Some(scenes) = &self.scenes {
            if self.variants.is_some() {
                return Err(OpError::invalid("scenes", "new scenes need every variant blessed again: leave out `variants`"));
            }
            manifest.scenes.clone_from(scenes);
            manifest.variants.clear();
        } else if manifest.scenes.is_empty() {
            manifest.scenes = GoldenScene::defaults();
        }
        manifest.check()?;
        let set = variants(doc, &self.character)?;
        if set.keys().any(|id| id.as_str() == BASE) {
            return Err(OpError::invalid(
                "character",
                format!("a variant is called '{BASE}', the name goldens give the character's own definition"),
            )
            .hint("rename the variant"));
        }
        let names: Vec<String> = match &self.variants {
            Some(names) => names.clone(),
            None => std::iter::once(BASE.to_owned()).chain(set.keys().map(ToString::to_string)).collect(),
        };
        if names.is_empty() {
            return Err(OpError::invalid("variants", "no variants to bless").hint("leave `variants` out to bless them all"));
        }
        let mut targets = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            let (variant, game) = if name == BASE {
                (None, false)
            } else {
                let (id, def) = set.iter().find(|(id, _)| id.as_str() == name).ok_or_else(|| {
                    let known: Vec<String> = set.keys().map(ToString::to_string).collect();
                    let mut error = OpError::not_found(format!("character '{}' has no variant '{name}'", self.character))
                        .hint(format!("its variants are {BASE}, {}", known.join(", ")));
                    error.field = Some(format!("variants/{i}"));
                    error
                })?;
                (Some(id.clone()), is_game_face(id, def))
            };
            // A variant blessed before keeps its own choice; a new one is pixel-exact if it is the game face.
            let pixel_exact = manifest.variants.get(name).map_or(game, |golden| golden.pixel_exact);
            targets.push(Target { name: name.clone(), variant, pixel_exact });
        }
        Ok((targets, manifest))
    }

    /// The preview that renders `variant` in `scene`.
    fn preview(&self, variant: Option<&VariantId>, scene: &GoldenScene) -> Preview {
        golden_preview(&self.character, variant, scene)
    }
}

/// The `render.preview` a golden of `variant` in `scene` is: its shot from the front of the opening scene, at offline
/// quality, which gives the same bytes on every run.
pub(super) fn golden_preview(character: &CharId, variant: Option<&VariantId>, scene: &GoldenScene) -> Preview {
    Preview {
        character: character.clone(),
        variant: variant.cloned(),
        scene: None,
        camera: None,
        framing: Some(scene.framing),
        yaw: Some(scene.yaw),
        pitch: Some(scene.pitch),
        zoom: None,
        light_rig: Some(scene.light_rig.clone()),
        expression: scene.expression.clone(),
        weight: 1.0,
        pose: scene.pose.clone(),
        size: scene.size,
        quality: RenderQuality::Offline,
        preset: None,
    }
}

/// `scene`'s render of `variant`: the stored PNG's hash.
pub(super) fn render(job: &JobCtx<'_>, preview: &Preview) -> Result<String, OpError> {
    let result: PreviewResult = serde_json::from_value(preview.work(job)?)
        .map_err(|e| OpError::internal(format!("reading a golden's preview: {e}")))?;
    Ok(result.hash)
}

/// The render count of `variants` blessed or checked in `scenes`.
pub(super) fn count(variants: usize, scenes: usize) -> u32 {
    u32::try_from(variants * scenes).unwrap_or(u32::MAX)
}

impl Op for GoldenBless {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.golden.bless",
            summary: "Render a character's variants (and its base) in its golden shots and record them in the project's \
                      golden manifest (goldens/<character>), so render.golden.check can hold later renders to them. The \
                      game face is pixel-exact by default; the others compare within each shot's tolerance.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // The manifest is a document table, so the patch's inverse undoes a bless.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (targets, manifest) = self.prepare(ctx.doc)?;
        let renders = count(targets.len(), manifest.scenes.len());
        let largest = manifest.scenes.values().map(|s| omuzaki_core::glam::UVec2::from(s.size)).fold(
            omuzaki_core::glam::UVec2::ZERO,
            omuzaki_core::glam::UVec2::max,
        );
        let output = serde_json::json!({ "renders": renders, "variants": targets.iter().map(|t| &t.name).collect::<Vec<_>>() });
        Ok(Plan {
            output: Some(output),
            cost: Some(cost(largest, renders, RenderQuality::Offline, 0)),
            ..Plan::default()
        })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let (targets, mut manifest) = job.read(|doc| self.prepare(doc))?;
        let total = count(targets.len(), manifest.scenes.len());
        let mut done = 0_u32;
        for target in &targets {
            let definition = job.read(|doc| definition(doc, &self.character, target.variant.as_ref()))?;
            let mut images = BTreeMap::new();
            for (name, scene) in &manifest.scenes {
                #[allow(clippy::cast_precision_loss, reason = "a few hundred renders at most")]
                job.checkpoint("rendering", done as f32 / total.max(1) as f32)?;
                images.insert(name.clone(), render(job, &self.preview(target.variant.as_ref(), scene))?);
                done += 1;
            }
            manifest
                .variants
                .insert(target.name.clone(), GoldenVariant { pixel_exact: target.pixel_exact, definition, images });
        }
        job.checkpoint("done", 1.0)?;
        let result = GoldenBlessResult { manifest, blessed: targets.into_iter().map(|t| t.name).collect(), renders: done };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the bless: {e}")))
    }

    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        let blessed: GoldenBlessResult = serde_json::from_value(result.clone())
            .map_err(|e| OpError::internal(format!("reading the bless back: {e}")))?;
        // The goldens are of the document the job read; if a blessed variant changed since, they are of no document.
        for name in &blessed.blessed {
            let variant = (name != BASE).then(|| VariantId::new(name)).transpose().map_err(|e| OpError::internal(e.to_string()))?;
            let now = definition(ctx.doc, &self.character, variant.as_ref())?;
            if blessed.manifest.variants.get(name).is_none_or(|golden| golden.definition != now) {
                return Err(OpError::conflict(format!("'{name}' changed while it was being rendered"))
                    .hint("bless again once the edits are done"));
            }
        }
        let value = serde_json::to_value(&blessed.manifest)
            .map_err(|e| OpError::internal(format!("serialising the manifest: {e}")))?;
        let at = manifest_path(&self.character);
        let patch = if ctx.doc.get(&at) == Some(&value) { omuzaki_doc_api::Patch::new() } else { put(ctx.doc, &at, value) };
        Ok(Plan { patch, output: Some(result), ..Plan::default() })
    }
}

register_op!(GoldenBless);
