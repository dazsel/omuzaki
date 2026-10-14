//! Materials, textures, images and samplers.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Extensions, collect};

/// A material: PBR metallic-roughness plus the slots every glTF material has.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Material {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The metallic-roughness model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pbr_metallic_roughness: Option<Pbr>,
    /// Tangent-space normal map (`scale` is the strength).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal_texture: Option<TextureInfo>,
    /// Baked ambient occlusion (`strength` is the amount).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occlusion_texture: Option<TextureInfo>,
    /// Emitted light.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_texture: Option<TextureInfo>,
    /// Emitted colour factor, within 0..1 (brighter colours use `KHR_materials_emissive_strength`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_factor: Option<[f32; 3]>,
    /// `OPAQUE`, `MASK` or `BLEND`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_mode: Option<String>,
    /// The `MASK` threshold (default 0.5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_cutoff: Option<f32>,
    /// Whether back faces are drawn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_sided: Option<bool>,
    /// Extensions: the typed ones, and the rest raw.
    #[serde(skip_serializing_if = "MaterialExtensions::is_empty")]
    pub extensions: MaterialExtensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

impl Material {
    pub(super) fn add_extension_names(&self, names: &mut BTreeSet<String>) {
        if self.extensions.unlit.is_some() {
            names.insert("KHR_materials_unlit".into());
        }
        if self.extensions.emissive_strength.is_some() {
            names.insert("KHR_materials_emissive_strength".into());
        }
        collect(&self.extensions.others, names);
        let pbr = self.pbr_metallic_roughness.as_ref();
        if let Some(pbr) = pbr {
            collect(&pbr.extensions, names);
        }
        let slots = [
            pbr.and_then(|p| p.base_color_texture.as_ref()),
            pbr.and_then(|p| p.metallic_roughness_texture.as_ref()),
            self.normal_texture.as_ref(),
            self.occlusion_texture.as_ref(),
            self.emissive_texture.as_ref(),
        ];
        for info in slots.into_iter().flatten() {
            info.add_extension_names(names);
        }
    }
}

/// A material's extensions: the two this crate models, typed so floats keep their `f32` text on write, and the rest
/// raw. Extensions with their own texture references (`VRMC_materials_mtoon`) stay raw; parse their bodies with
/// [`TextureInfo`] where a texture reference sits.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaterialExtensions {
    /// `KHR_materials_emissive_strength`.
    #[serde(rename = "KHR_materials_emissive_strength", skip_serializing_if = "Option::is_none")]
    pub emissive_strength: Option<EmissiveStrength>,
    /// `KHR_materials_unlit`.
    #[serde(rename = "KHR_materials_unlit", skip_serializing_if = "Option::is_none")]
    pub unlit: Option<Empty>,
    /// Every other extension, verbatim.
    #[serde(flatten)]
    pub others: Extensions,
}

impl MaterialExtensions {
    /// True if the material has no extensions at all.
    pub fn is_empty(&self) -> bool {
        self.emissive_strength.is_none() && self.unlit.is_none() && self.others.is_empty()
    }
}

/// An extension object with no members (`KHR_materials_unlit`).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

/// `KHR_materials_emissive_strength`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EmissiveStrength {
    /// Multiplier on the emissive colour (default 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_strength: Option<f32>,
}

/// The metallic-roughness model.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Pbr {
    /// Base colour factor, linear RGBA (default white).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_color_factor: Option<[f32; 4]>,
    /// Base colour texture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_color_texture: Option<TextureInfo>,
    /// Metallic factor (default 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metallic_factor: Option<f32>,
    /// Roughness factor (default 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness_factor: Option<f32>,
    /// Metallic (blue) and roughness (green) texture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metallic_roughness_texture: Option<TextureInfo>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// A texture slot: `textureInfo`, plus `scale` for normal textures and `strength` for occlusion textures. Also the
/// shape of the texture references inside extension bodies such as `VRMC_materials_mtoon` (which adds nothing to
/// it), so those can be parsed with this type.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TextureInfo {
    /// The texture.
    pub index: usize,
    /// Which UV set it samples (default 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tex_coord: Option<u32>,
    /// Normal map strength.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Occlusion amount.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strength: Option<f32>,
    /// Extensions: `KHR_texture_transform` typed, the rest raw.
    #[serde(skip_serializing_if = "TextureInfoExtensions::is_empty")]
    pub extensions: TextureInfoExtensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

impl TextureInfo {
    fn add_extension_names(&self, names: &mut BTreeSet<String>) {
        if self.extensions.transform.is_some() {
            names.insert("KHR_texture_transform".into());
        }
        collect(&self.extensions.others, names);
    }
}

/// A texture slot's extensions.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextureInfoExtensions {
    /// `KHR_texture_transform`.
    #[serde(rename = "KHR_texture_transform", skip_serializing_if = "Option::is_none")]
    pub transform: Option<TextureTransform>,
    /// Every other extension, verbatim.
    #[serde(flatten)]
    pub others: Extensions,
}

impl TextureInfoExtensions {
    /// True if the slot has no extensions at all.
    pub fn is_empty(&self) -> bool {
        self.transform.is_none() && self.others.is_empty()
    }
}

/// `KHR_texture_transform`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TextureTransform {
    /// UV offset (default 0, 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f32; 2]>,
    /// Rotation in radians about the UV origin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f32>,
    /// UV scale (default 1, 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 2]>,
    /// Overrides the slot's UV set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tex_coord: Option<u32>,
}

/// A texture: an image sampled with a sampler.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Texture {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The sampler; none means the viewer's defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampler: Option<usize>,
    /// The image. Textures that only name an extension source (`KHR_texture_basisu`, `EXT_texture_webp`) have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<usize>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// An image, by `uri` or buffer view.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Image {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// A relative path or `data:` URI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The declared media type (often wrong or missing; readers go by the file signature).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The buffer view holding the encoded image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffer_view: Option<usize>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// Texture sampling state. Values are the OpenGL codes in [`code`](super::code).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sampler {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Magnification filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mag_filter: Option<u32>,
    /// Minification filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_filter: Option<u32>,
    /// Wrap mode along U.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap_s: Option<u32>,
    /// Wrap mode along V.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap_t: Option<u32>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}
