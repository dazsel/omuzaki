//! A project's golden manifest (spec VF-03): for one character, the shots it is checked in and what each of its
//! variants looked like in them when blessed. It lives in the document at `goldens/<character>`, so it is saved, undone
//! and diffed with the project; the images it names are in the content store by hash.
//!
//! ```toml
//! # goldens/kit.toml
//! [scenes.full]
//! framing = "full"
//! size = [256, 384]
//! tolerance = { per_channel = 2, max_changed = 0.001 }
//!
//! [variants.game]
//! pixel_exact = true                # the game face: byte for byte
//! definition = "9f2c..."            # what the variant's render depends on, hashed
//! images = { full = "e3b0..." }
//! ```

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, VariantDef, VariantId, VariantSet, paths};
use omuzaki_core::id::Slug;
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::OpError;
use omuzaki_stage_api::Framing;
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ops::render::job::{DEFAULT_LIGHT_RIG, RenderQuality, check_size, still_light};
use crate::ops::render::pose::PoseArg;
use crate::scene::default::scene_for;

/// The document table every character's manifest is under.
pub const GOLDENS: &str = "goldens";
/// The name the character's own definition, with no variant applied, goes by in a manifest.
pub const BASE: &str = "base";
/// The most shots a manifest checks a character in.
pub const MAX_SCENES: usize = 16;

/// Where `character`'s manifest is.
#[must_use]
pub fn manifest_path(character: &CharId) -> DocPath {
    DocPath::root().join(GOLDENS).join(character.as_str())
}

/// One character's goldens.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoldenManifest {
    /// The shots every variant is checked in, by name.
    pub scenes: BTreeMap<String, GoldenScene>,
    /// Each blessed variant by id, and the character's own definition as `base`.
    #[serde(default)]
    pub variants: BTreeMap<String, GoldenVariant>,
}

fn default_size() -> [u32; 2] {
    [256, 384]
}

fn default_light_rig() -> String {
    DEFAULT_LIGHT_RIG.to_owned()
}

/// One shot of a manifest: the camera, light, face, pose and size, and how far a render may drift from its golden.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoldenScene {
    /// `full` (the default), `bust`, `face` or `hands`, from the front of the project's opening scene.
    #[serde(default)]
    pub framing: Framing,
    /// Degrees around the character, -180 to 180.
    #[serde(default)]
    pub yaw: f32,
    /// Degrees from above, -90 to 90.
    #[serde(default)]
    pub pitch: f32,
    /// The light rig (`viewer-studio` by default).
    #[serde(default = "default_light_rig")]
    pub light_rig: String,
    /// An expression of the face profile; the neutral face when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<ExpressionId>,
    /// A pose of the pose library; the character as sculpted when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<PoseArg>,
    /// Width and height in pixels (256 x 384 by default).
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// How far a render of a variant that isn't pixel-exact may drift from its golden.
    #[serde(default)]
    pub tolerance: GoldenTolerance,
}

/// How far a render may drift from its golden and still pass.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoldenTolerance {
    /// A pixel has changed when a channel (premultiplied, alpha included) differs by more than this, 0 to 255.
    pub per_channel: u8,
    /// The render passes when at most this share of its pixels changed, 0 to 1.
    pub max_changed: f64,
}

impl Default for GoldenTolerance {
    /// Two levels of rounding, on a thousandth of the picture: what a GPU varies by between runs and drivers.
    fn default() -> Self {
        Self { per_channel: 2, max_changed: 0.001 }
    }
}

/// What one variant looked like when blessed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoldenVariant {
    /// Every render must be the golden byte for byte: true by default for the game face (the variant for the widest
    /// shots), which the other variants' edits must never touch.
    pub pixel_exact: bool,
    /// The hash of everything the variant's render depends on in the document (the character, the variant's chain of
    /// overrides, the opening scene) when it was blessed. A check whose hash is the same knows the render is too.
    pub definition: String,
    /// The golden of each scene: the stored render's hash.
    pub images: BTreeMap<String, String>,
}

impl GoldenScene {
    /// The two shots a manifest starts with: the whole character, and the face up close.
    #[must_use]
    pub fn defaults() -> BTreeMap<String, Self> {
        let shot = |framing, size| Self {
            framing,
            yaw: 0.0,
            pitch: 0.0,
            light_rig: default_light_rig(),
            expression: None,
            pose: None,
            size,
            tolerance: GoldenTolerance::default(),
        };
        BTreeMap::from([("face".to_owned(), shot(Framing::Face, [256, 256])), ("full".to_owned(), shot(Framing::Full, default_size()))])
    }

    /// Checks every value, naming `field` and the part at fault.
    ///
    /// # Errors
    /// `Invalid` for a size, angle, rig, pose or tolerance out of range.
    pub fn check(&self, field: &str) -> Result<(), OpError> {
        let at = |part: &str| format!("{field}/{part}");
        let rename = |part: &str| {
            let to = at(part);
            move |mut e: OpError| {
                e.field = Some(to);
                e
            }
        };
        check_size(self.size, RenderQuality::Offline).map_err(rename("size"))?;
        if self.framing == Framing::Free {
            return Err(OpError::invalid(at("framing"), "a golden is framed: face, bust, full or hands"));
        }
        if !((-180.0..=180.0).contains(&self.yaw) && (-90.0..=90.0).contains(&self.pitch)) {
            return Err(OpError::invalid(
                at("yaw"),
                format!("yaw {} and pitch {} must be in -180 to 180 and -90 to 90", self.yaw, self.pitch),
            ));
        }
        still_light(&self.light_rig).map_err(rename("light_rig"))?;
        if let Some(pose) = &self.pose {
            pose.check(&at("pose"))?;
        }
        self.tolerance.check(&at("tolerance"))
    }
}

impl GoldenTolerance {
    /// Checks the share is a number from 0 to 1.
    ///
    /// # Errors
    /// `Invalid` naming `field/max_changed`.
    pub fn check(&self, field: &str) -> Result<(), OpError> {
        if (0.0..=1.0).contains(&self.max_changed) {
            Ok(())
        } else {
            Err(OpError::invalid(
                format!("{field}/max_changed"),
                format!("max_changed {} is not a share of the picture from 0 to 1", self.max_changed),
            ))
        }
    }
}

impl GoldenManifest {
    /// `character`'s manifest from the document, if it has one.
    ///
    /// # Errors
    /// `Invalid` naming the manifest's path when it doesn't read as one or fails [`GoldenManifest::check`].
    pub fn read(doc: &dyn DocRead, character: &CharId) -> Result<Option<Self>, OpError> {
        let path = manifest_path(character);
        let Some(node) = doc.get(&path) else { return Ok(None) };
        let manifest: Self = serde_json::from_value(node.clone())
            .map_err(|e| OpError::invalid(path.to_string(), format!("the golden manifest doesn't read: {e}")))?;
        manifest.check()?;
        Ok(Some(manifest))
    }

    /// Checks the scene names, every scene, and that each variant has a golden of every scene.
    ///
    /// # Errors
    /// `Invalid` naming the first value at fault.
    pub fn check(&self) -> Result<(), OpError> {
        if self.scenes.is_empty() || self.scenes.len() > MAX_SCENES {
            return Err(OpError::invalid("scenes", format!("{} scenes; a manifest has 1 to {MAX_SCENES}", self.scenes.len())));
        }
        for (name, scene) in &self.scenes {
            Slug::new(name.as_str())
                .map_err(|e| OpError::invalid(format!("scenes/{name}"), format!("'{name}' is not a scene name: {e}")))?;
            scene.check(&format!("scenes/{name}"))?;
        }
        for (variant, golden) in &self.variants {
            for (scene, hash) in &golden.images {
                if !self.scenes.contains_key(scene) {
                    return Err(OpError::invalid(format!("variants/{variant}/images/{scene}"), "a golden of no scene"));
                }
                hash.parse::<ContentHash>().map_err(|_| {
                    OpError::invalid(format!("variants/{variant}/images/{scene}"), format!("'{hash}' is not a hash"))
                })?;
            }
        }
        Ok(())
    }
}

/// The character's variants, by id.
///
/// # Errors
/// `Invalid` when the variants don't read.
pub fn variants(doc: &dyn DocRead, character: &CharId) -> Result<VariantSet, OpError> {
    let Some(node) = doc.get(&paths::variants(character)) else { return Ok(VariantSet::new()) };
    serde_json::from_value(node.clone())
        .map_err(|e| OpError::invalid("character", format!("the variants of '{character}' don't read: {e}")))
}

/// Whether `def` is the game face: the variant for the widest shots, or the one called `game`.
#[must_use]
pub fn is_game_face(id: &VariantId, def: &VariantDef) -> bool {
    id.as_str() == "game" || def.shot.is_some_and(|shot| shot.min <= 0.0)
}

/// The hash of what `variant`'s render reads from the document: the character without its variants, the chain of
/// variants it extends from itself down, and the opening scene it is shown in. Two documents with the same hash render
/// it the same.
///
/// # Errors
/// `NotFound` for a character or variant that isn't there; what [`scene_for`] reports.
pub fn definition(doc: &dyn DocRead, character: &CharId, variant: Option<&VariantId>) -> Result<String, OpError> {
    let mut node = doc
        .get(&paths::character(character))
        .cloned()
        .ok_or_else(|| OpError::not_found(format!("there is no character '{character}'")))?;
    if let Value::Object(table) = &mut node {
        table.remove("variants");
    }
    let mut chain: Vec<(VariantId, VariantDef)> = Vec::new();
    if let Some(variant) = variant {
        let set = variants(doc, character)?;
        let mut next = Some(variant.clone());
        while let Some(id) = next.take() {
            // A loop is the build's to refuse; a variant met twice ends the chain here.
            if chain.iter().any(|(seen, _)| *seen == id) {
                break;
            }
            let def = set
                .get(&id)
                .ok_or_else(|| OpError::not_found(format!("character '{character}' has no variant '{id}'")))?;
            next.clone_from(&def.extends);
            chain.push((id, def.clone()));
        }
    }
    let (_, scene) = scene_for(doc, None)?;
    let chain: Vec<Value> = chain.iter().map(|(id, def)| serde_json::json!([id, def])).collect();
    let all = serde_json::json!({ "character": node, "variants": chain, "scene": scene });
    let bytes = serde_json::to_vec(&all).map_err(|e| OpError::internal(format!("hashing the definition: {e}")))?;
    Ok(ContentHash::of(&bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_manifest_round_trips_and_checks() {
        let manifest = GoldenManifest { scenes: GoldenScene::defaults(), variants: BTreeMap::new() };
        manifest.check().expect("checks");
        let value = serde_json::to_value(&manifest).expect("serialises");
        assert_eq!(serde_json::from_value::<GoldenManifest>(value).expect("reads"), manifest);
    }

    #[test]
    fn a_tolerance_outside_the_picture_is_refused() {
        let mut scenes = GoldenScene::defaults();
        scenes.get_mut("full").expect("full").tolerance.max_changed = 1.5;
        let error = GoldenManifest { scenes, variants: BTreeMap::new() }.check().expect_err("refused");
        assert_eq!(error.field.as_deref(), Some("scenes/full/tolerance/max_changed"));
    }
}
