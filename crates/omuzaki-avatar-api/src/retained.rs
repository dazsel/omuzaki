//! What export needs and the runtime does not use, kept exactly as the file had it (design D8).
//!
//! The runtime [`MaterialSpec`](omuzaki_char_api::MaterialSpec) holds 8-bit sRGB colours, so converting back would
//! nudge every value (linear 0.5 comes back as about 0.502886). [`Retained`] therefore keeps every source material as
//! the file had it (the io-api [`Material`], linear f32 and the whole `MToon` block), and the gather writes that,
//! never a value converted back from the runtime. A person's edit to a material is an explicit field of
//! [`AvatarDef::materials`](crate::def::AvatarDef::materials), applied field by field over the retained material.
//!
//! Everything here serialises (the prepared avatar may be cached in the store), and serde round trips are bit for bit.

use omuzaki_anim_api::BoneId;
use omuzaki_char_api::MaterialName;
use omuzaki_io_api::{ImageMime, Material, Passthrough, Texture, VrmMeta};
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One source material, under the runtime name the importer gave it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedMaterial {
    /// The name of the runtime material made from it (`ImportedAvatar::materials`).
    pub name: MaterialName,
    /// The material exactly as the file had it, its texture references indexing [`Retained::textures`].
    pub source: Material,
}

impl RetainedMaterial {
    /// `source`, kept under the runtime name `name`.
    pub fn new(name: MaterialName, source: Material) -> Self {
        Self { name, source }
    }
}

/// One source image: its bytes are in the store, under `hash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedImage {
    /// The image's name in the file.
    pub name: String,
    /// Its encoding.
    pub mime: ImageMime,
    /// Its encoded bytes, unchanged, in the store.
    pub hash: ContentHash,
}

impl RetainedImage {
    /// The image `name`, encoded as `mime`, stored under `hash`.
    pub fn new(name: impl Into<String>, mime: ImageMime, hash: ContentHash) -> Self {
        Self { name: name.into(), mime, hash }
    }
}

/// One node of the file, in file order: its name and what the importer did not interpret on it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedNode {
    /// The node's name in the file (not unique; skeleton bones have sanitised names).
    pub name: String,
    /// The skeleton bone made from it, if any: the node-to-bone table export writes back through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<BoneId>,
    /// Unknown extensions and extras on the node, verbatim.
    #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
    pub passthrough: Passthrough,
}

impl RetainedNode {
    /// A node called `name`, made into no bone, carrying nothing.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }

    /// Records the bone made from it (builder style).
    #[must_use]
    pub fn with_bone(mut self, bone: BoneId) -> Self {
        self.bone = Some(bone);
        self
    }
}

/// What an imported avatar carries for export only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Retained {
    /// Every source material, in file order.
    #[serde(default)]
    pub materials: Vec<RetainedMaterial>,
    /// The file's textures (an image and its sampler), in file order: what the materials' texture references index.
    #[serde(default)]
    pub textures: Vec<Texture>,
    /// The file's images, in file order: what the textures and the meta's thumbnail index.
    #[serde(default)]
    pub images: Vec<RetainedImage>,
    /// Every node of the file, in file order.
    #[serde(default)]
    pub nodes: Vec<RetainedNode>,
    /// The VRM meta (licence, authors, the thumbnail as an index into `images`), or `None` for a plain glTF. Its
    /// 0.x extras, which io-api's own serialisation leaves out, are kept too ([`meta_serde`]).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "meta_serde")]
    pub meta: Option<VrmMeta>,
    /// Unknown extensions and extras on the file as a whole, verbatim.
    #[serde(default, skip_serializing_if = "Passthrough::is_empty")]
    pub passthrough: Passthrough,
}

impl Retained {
    /// Nothing retained: what an avatar built by hand starts with.
    pub fn new() -> Self {
        Self::default()
    }

    /// The source material of the runtime material `name`.
    pub fn material(&self, name: &MaterialName) -> Option<&Material> {
        self.materials.iter().find(|m| &m.name == name).map(|m| &m.source)
    }

    /// The thumbnail image, if the meta names one that exists.
    pub fn thumbnail(&self) -> Option<&RetainedImage> {
        let index = self.meta.as_ref()?.thumbnail_image?;
        self.images.get(index.index())
    }
}

/// Serialises the meta with its VRM 0.x extras: `VrmMeta` skips `extras_vrm0` (they are not part of the 1.0 JSON), but
/// a cached avatar must re-export a 0.x file with every key it had, so they are written beside it.
mod meta_serde {
    use super::{Deserialize, Map, Serialize, Value, VrmMeta};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Written {
        vrm1: VrmMeta,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        vrm0_extras: Map<String, Value>,
    }

    #[allow(clippy::ref_option)] // serde's `with` hands the field over by reference
    pub(super) fn serialize<S: serde::Serializer>(meta: &Option<VrmMeta>, serializer: S) -> Result<S::Ok, S::Error> {
        meta.as_ref()
            .map(|m| Written { vrm1: m.clone(), vrm0_extras: m.extras_vrm0.clone() })
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<VrmMeta>, D::Error> {
        Ok(Option::<Written>::deserialize(deserializer)?.map(|w| {
            let mut meta = w.vrm1;
            meta.extras_vrm0 = w.vrm0_extras;
            meta
        }))
    }
}
