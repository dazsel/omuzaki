//! `render.golden.check`: after an edit, renders every blessed variant of a character in its golden shots again and
//! holds each render to its golden (spec VF-03): a pixel-exact variant (the game face) byte for byte, the others within
//! the shot's tolerance. A variant whose definition hasn't changed since the bless renders as it did, so it is not
//! rendered again: calling the check after every edit to a shared part costs only the variants the edit reached.

use omuzaki_char_api::{CharId, VariantId};
use omuzaki_doc_api::DocRead;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::bless::{golden_preview, render};
use super::manifest::{BASE, GoldenManifest, GoldenVariant, definition, variants};
use crate::ops::render::compare::{Compare, CompareMode, CompareResult, Difference};
use crate::ops::render::job::{RenderQuality, cost};
use crate::ops::render::picture::{RENDER_URI, load};

/// Re-renders a character's blessed variants and holds them to their goldens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "intended": ["close-up"], "rerender": true}
    ],
    "x-omuzaki-fixture" = "goldens"
))]
pub struct GoldenCheck {
    /// The character.
    pub character: CharId,
    /// The variants the edit means to change, by id (`base` for the character's own definition): their changes are
    /// reported, and don't fail the check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intended: Vec<String>,
    /// Render every variant, even one whose definition is as it was blessed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rerender: bool,
}

/// How one render held up against its golden.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Not rendered: the variant's definition is as it was blessed, so its render is too.
    Unchanged,
    /// Not rendered: the variant has no golden of this shot yet (`render.golden.bless` records one).
    Unblessed,
    /// Rendered, and pixel for pixel the golden.
    Same,
    /// Rendered, different, and within the shot's tolerance (never for a pixel-exact variant).
    Within,
    /// Rendered and changed, on a variant the edit meant to change: reported, not failed.
    Intended,
    /// Rendered and changed past what the variant allows: the check fails.
    Changed,
}

/// One render's report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SceneReport {
    /// The shot's name in the manifest.
    pub scene: String,
    /// How it held up.
    pub outcome: Outcome,
    /// The golden, `omuzaki://render/<hash>.png`, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub golden: Option<String>,
    /// The new render, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<String>,
    /// The share of the pixels that changed by more than the tolerance's `per_channel` (any change at all for a
    /// pixel-exact variant), 0 to 1.
    pub changed: f32,
    /// The largest channel difference, 0 to 255.
    pub max_difference: u8,
    /// For a change: the box around it, `[x, y, width, height]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[u32; 4]>,
    /// For a change: the golden dimmed, with the changed pixels in yellow to red.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heatmap: Option<String>,
    /// For a change: each channel's difference, brightened, on black.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// One variant's report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VariantReport {
    /// The variant's id, `base` for the character's own definition.
    pub variant: String,
    /// Whether it must match byte for byte.
    pub pixel_exact: bool,
    /// Whether the edit meant to change it.
    pub intended: bool,
    /// Each shot's report.
    pub scenes: Vec<SceneReport>,
}

/// What the check found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GoldenCheckResult {
    /// True when no render changed past what its variant allows.
    pub passed: bool,
    /// How many pictures were rendered.
    pub renders: u32,
    /// Each blessed variant, in the manifest's order.
    pub variants: Vec<VariantReport>,
    /// The character's variants (and `base`) with no goldens yet (`render.golden.bless` them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unblessed: Vec<String>,
}

/// A blessed variant to check.
struct Target {
    name: String,
    variant: Option<VariantId>,
    golden: GoldenVariant,
    stale: bool,
}

impl GoldenCheck {
    /// The manifest, the variants to check with whether each needs rendering, and the variants with no goldens.
    fn prepare(&self, doc: &dyn DocRead) -> Result<(GoldenManifest, Vec<Target>, Vec<String>), OpError> {
        let manifest = GoldenManifest::read(doc, &self.character)?.ok_or_else(|| {
            OpError::not_found(format!("character '{}' has no goldens", self.character))
                .hint("render.golden.bless records them")
        })?;
        let set = variants(doc, &self.character)?;
        let names: Vec<String> = std::iter::once(BASE.to_owned()).chain(set.keys().map(ToString::to_string)).collect();
        for (i, name) in self.intended.iter().enumerate() {
            if !names.contains(name) {
                let mut error = OpError::not_found(format!("character '{}' has no variant '{name}'", self.character))
                    .hint(format!("its variants are {}", names.join(", ")));
                error.field = Some(format!("intended/{i}"));
                return Err(error);
            }
        }
        let mut targets = Vec::with_capacity(manifest.variants.len());
        for (name, golden) in &manifest.variants {
            let variant = if name == BASE {
                None
            } else {
                let id = set.keys().find(|id| id.as_str() == name).cloned().ok_or_else(|| {
                    OpError::not_found(format!("the blessed variant '{name}' is gone from character '{}'", self.character))
                        .hint("bless the character again")
                })?;
                Some(id)
            };
            let now = definition(doc, &self.character, variant.as_ref())?;
            targets.push(Target { name: name.clone(), variant, golden: golden.clone(), stale: self.rerender || now != golden.definition });
        }
        let unblessed = std::iter::once(BASE.to_owned())
            .chain(set.keys().map(ToString::to_string))
            .filter(|name| !manifest.variants.contains_key(name))
            .collect();
        Ok((manifest, targets, unblessed))
    }
}

/// How many pictures checking `targets` renders: a shot of each variant that changed, where it has a golden.
fn renders_of(targets: &[Target]) -> u32 {
    let n: usize = targets.iter().filter(|t| t.stale).map(|t| t.golden.images.len()).sum();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// `hash` as a render URI.
fn uri(hash: &str) -> String {
    format!("{RENDER_URI}{hash}.png")
}

impl Op for GoldenCheck {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.golden.check",
            summary: "Render a character's blessed variants again in their golden shots and hold each to its golden: \
                      the game face byte for byte, the others within the shot's tolerance. A change on a variant in \
                      `intended` is reported, not failed; a failing render comes with a heatmap and a difference \
                      picture. Variants whose definition is unchanged since the bless are not rendered again.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // It changes nothing in the document (the renders go to the content store).
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (manifest, targets, unblessed) = self.prepare(ctx.doc)?;
        let renders = renders_of(&targets);
        let largest = manifest.scenes.values().map(|s| omuzaki_core::glam::UVec2::from(s.size)).fold(
            omuzaki_core::glam::UVec2::ZERO,
            omuzaki_core::glam::UVec2::max,
        );
        let output = serde_json::json!({
            "renders": renders,
            "variants": targets.iter().filter(|t| t.stale).map(|t| &t.name).collect::<Vec<_>>(),
            "unchanged": targets.iter().filter(|t| !t.stale).map(|t| &t.name).collect::<Vec<_>>(),
            "unblessed": unblessed,
        });
        Ok(Plan { output: Some(output), cost: Some(cost(largest, renders, RenderQuality::Offline, 0)), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let (manifest, targets, unblessed) = job.read(|doc| self.prepare(doc))?;
        let total = renders_of(&targets);
        let mut renders = 0_u32;
        let mut reports = Vec::with_capacity(targets.len());
        for target in &targets {
            let intended = self.intended.contains(&target.name);
            let mut scenes = Vec::with_capacity(manifest.scenes.len());
            for (name, scene) in &manifest.scenes {
                let mut report = SceneReport {
                    scene: name.clone(),
                    outcome: Outcome::Unblessed,
                    golden: None,
                    render: None,
                    changed: 0.0,
                    max_difference: 0,
                    bounds: None,
                    heatmap: None,
                    diff: None,
                };
                let Some(golden) = target.golden.images.get(name) else {
                    scenes.push(report);
                    continue;
                };
                (report.outcome, report.golden) = (Outcome::Unchanged, Some(uri(golden)));
                if target.stale {
                    #[allow(clippy::cast_precision_loss, reason = "a few hundred renders at most")]
                    job.checkpoint("rendering", renders as f32 / total.max(1) as f32)?;
                    let hash = render(job, &golden_preview(&self.character, target.variant.as_ref(), scene))?;
                    renders += 1;
                    report.render = Some(uri(&hash));
                    report.outcome = if hash == *golden {
                        Outcome::Same
                    } else {
                        let threshold = if target.golden.pixel_exact { 0 } else { scene.tolerance.per_channel };
                        let field = format!("{}/{name}", target.name);
                        let (before, after) = (load(job, &field, golden)?, load(job, &field, &hash)?);
                        if before.size == after.size {
                            let difference = Difference::of(&before, &after, threshold);
                            #[allow(clippy::cast_precision_loss, reason = "a share needs no more than f32 holds")]
                            let share = difference.changed as f32 / (before.size.x * before.size.y).max(1) as f32;
                            (report.changed, report.max_difference, report.bounds) = (share, difference.max, difference.bounds);
                        } else {
                            // A render of another size changed everywhere.
                            (report.changed, report.max_difference) = (1.0, u8::MAX);
                        }
                        let allowed = if target.golden.pixel_exact { 0.0 } else { scene.tolerance.max_changed };
                        match (report.max_difference == 0, f64::from(report.changed) <= allowed) {
                            (true, _) => Outcome::Same,
                            (false, true) if !target.golden.pixel_exact => Outcome::Within,
                            _ if intended => Outcome::Intended,
                            _ => Outcome::Changed,
                        }
                    };
                    if matches!(report.outcome, Outcome::Changed | Outcome::Intended) && report.bounds.is_some() {
                        let picture = |mode| -> Result<String, OpError> {
                            let compare = Compare { a: golden.clone(), b: hash.clone(), mode, threshold: if target.golden.pixel_exact { 0 } else { scene.tolerance.per_channel } };
                            let result: CompareResult = serde_json::from_value(compare.work(job)?)
                                .map_err(|e| OpError::internal(format!("reading a comparison: {e}")))?;
                            Ok(result.uri)
                        };
                        report.heatmap = Some(picture(CompareMode::Heatmap)?);
                        report.diff = Some(picture(CompareMode::Diff)?);
                    }
                }
                scenes.push(report);
            }
            reports.push(VariantReport { variant: target.name.clone(), pixel_exact: target.golden.pixel_exact, intended, scenes });
        }
        job.checkpoint("done", 1.0)?;
        let passed = reports.iter().all(|v| v.scenes.iter().all(|s| s.outcome != Outcome::Changed));
        let result = GoldenCheckResult { passed, renders, variants: reports, unblessed };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the check: {e}")))
    }
}

register_op!(GoldenCheck);
