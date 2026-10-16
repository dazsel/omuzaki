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
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_io_api::{FirstPersonFlag, ImageMime, Material, Passthrough, Texture, VrmMeta};
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
    /// Its transform relative to its parent, as the file has it.
    #[serde(default, skip_serializing_if = "RetainedTransform::is_identity")]
    pub transform: RetainedTransform,
    /// Its children, as indices into [`Retained::nodes`], in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<u32>,
    /// The mesh drawn at it, an index into [`Retained::meshes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<u32>,
    /// The skin that deforms that mesh, an index into [`Retained::skins`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<u32>,
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

    /// Sets its transform (builder style).
    #[must_use]
    pub fn with_transform(mut self, transform: RetainedTransform) -> Self {
        self.transform = transform;
        self
    }

    /// Sets its children (builder style).
    #[must_use]
    pub fn with_children(mut self, children: Vec<u32>) -> Self {
        self.children = children;
        self
    }

    /// Draws the source mesh `mesh` at it, deformed by the skin `skin` if any (builder style).
    #[must_use]
    pub fn with_mesh(mut self, mesh: u32, skin: Option<u32>) -> Self {
        self.mesh = Some(mesh);
        self.skin = skin;
        self
    }
}

/// A node's transform as the file has it: translation, rotation, scale (glTF's TRS; a matrix is decomposed on import).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedTransform {
    /// Metres, relative to the parent.
    pub translation: Vec3,
    /// A unit quaternion.
    pub rotation: Quat,
    /// Per-axis scale.
    pub scale: Vec3,
}

impl RetainedTransform {
    /// No translation, no rotation, unit scale.
    pub const IDENTITY: Self = Self { translation: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::ONE };

    /// True for the identity, which serialisation leaves out.
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
}

impl Default for RetainedTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// One primitive of a source mesh: the runtime mesh it became and the source material it drew.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedPrimitive {
    /// The runtime mesh made from it, an index into `ImportedAvatar::meshes`.
    pub mesh: u16,
    /// Its material, an index into [`Retained::materials`]; `None` when the file gave it none (glTF's default
    /// material), so export writes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<u32>,
}

impl RetainedPrimitive {
    /// The primitive that became runtime mesh `mesh`, with no material in the file.
    pub const fn new(mesh: u16) -> Self {
        Self { mesh, material: None }
    }

    /// Records its material (builder style).
    #[must_use]
    pub const fn with_material(mut self, material: u32) -> Self {
        self.material = Some(material);
        self
    }
}

/// One skin of the file: its joints, their inverse bind matrices and its skeleton root, by node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedSkin {
    /// The skin's name in the file.
    pub name: String,
    /// Its joints, as indices into [`Retained::nodes`], in the file's order.
    pub joints: Vec<u32>,
    /// One inverse bind matrix per joint, exactly as the file has them; empty when the file gives none (identity).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inverse_bind: Vec<Mat4>,
    /// The skeleton root, an index into [`Retained::nodes`], when the file names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<u32>,
}

impl RetainedSkin {
    /// The skin `name` over the nodes `joints`, with `inverse_bind` (one per joint, or none).
    pub fn new(name: impl Into<String>, joints: Vec<u32>, inverse_bind: Vec<Mat4>) -> Self {
        Self { name: name.into(), joints, inverse_bind, skeleton: None }
    }

    /// Records its skeleton root (builder style).
    #[must_use]
    pub fn with_skeleton(mut self, skeleton: u32) -> Self {
        self.skeleton = Some(skeleton);
        self
    }
}

/// A first-person flag as the file writes it (`firstPerson.meshAnnotations[*].type`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RetainedFirstPerson {
    /// `auto`: split by head weights.
    Auto,
    /// `both`.
    Both,
    /// `thirdPersonOnly`.
    ThirdPersonOnly,
    /// `firstPersonOnly`.
    FirstPersonOnly,
}

impl From<FirstPersonFlag> for RetainedFirstPerson {
    fn from(flag: FirstPersonFlag) -> Self {
        match flag {
            FirstPersonFlag::Auto => Self::Auto,
            FirstPersonFlag::Both => Self::Both,
            FirstPersonFlag::ThirdPersonOnly => Self::ThirdPersonOnly,
            FirstPersonFlag::FirstPersonOnly => Self::FirstPersonOnly,
        }
    }
}

impl From<RetainedFirstPerson> for FirstPersonFlag {
    fn from(flag: RetainedFirstPerson) -> Self {
        match flag {
            RetainedFirstPerson::Auto => Self::Auto,
            RetainedFirstPerson::Both => Self::Both,
            RetainedFirstPerson::ThirdPersonOnly => Self::ThirdPersonOnly,
            RetainedFirstPerson::FirstPersonOnly => Self::FirstPersonOnly,
        }
    }
}

/// One first-person annotation of the file: a node and its flag.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedAnnotation {
    /// The node with the mesh, an index into [`Retained::nodes`].
    pub node: u32,
    /// Its flag.
    pub flag: RetainedFirstPerson,
}

impl RetainedAnnotation {
    /// The annotation of `node` with `flag`.
    pub const fn new(node: u32, flag: RetainedFirstPerson) -> Self {
        Self { node, flag }
    }
}

/// One mesh of the file. Its primitives became separate runtime meshes (one material each), and they share its
/// morph-target names: glTF names targets per mesh, not per primitive.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedMesh {
    /// The mesh's name in the file.
    pub name: String,
    /// Its primitives, in file order: the runtime mesh each became and its material.
    pub primitives: Vec<RetainedPrimitive>,
    /// Its morph-target names as the file has them (`extras.targetNames`, `Fcl_ALL_A`), one per target; empty when
    /// the file names none. The runtime names are slugs (`SparseMorph::name`), so these are what export writes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub morph_target_names: Vec<String>,
}

impl RetainedMesh {
    /// The mesh `name` with `primitives`, with no target names.
    pub fn new(name: impl Into<String>, primitives: Vec<RetainedPrimitive>) -> Self {
        Self { name: name.into(), primitives, morph_target_names: Vec::new() }
    }

    /// Sets its morph-target names (builder style).
    #[must_use]
    pub fn with_morph_target_names(mut self, names: Vec<String>) -> Self {
        self.morph_target_names = names;
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
    /// Every node of the file, in file order, with its transform, children, mesh and skin: the scene graph export
    /// rebuilds.
    #[serde(default)]
    pub nodes: Vec<RetainedNode>,
    /// The nodes with no parent that make up the default scene, as indices into `nodes`, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<u32>,
    /// The file's meshes, in file order: which runtime meshes each one's primitives became, and its morph-target
    /// names, so export rebuilds each mesh with its primitives and its one `extras.targetNames` table. Empty for an
    /// avatar that has none to keep.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meshes: Vec<RetainedMesh>,
    /// The file's skins, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skins: Vec<RetainedSkin>,
    /// The file's own first-person annotations (`VRMC_vrm.firstPerson`), by node and in file order; `None` when the
    /// file has no `firstPerson` block, so export tells "absent", "empty" and an explicit `auto` apart. The runtime
    /// list (`ImportedAvatar::first_person`) is resolved per mesh and cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_person: Option<Vec<RetainedAnnotation>>,
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
        meta.as_ref().map(|m| Written { vrm1: m.clone(), vrm0_extras: m.extras_vrm0.clone() }).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<VrmMeta>, D::Error> {
        Ok(Option::<Written>::deserialize(deserializer)?.map(|w| {
            let mut meta = w.vrm1;
            meta.extras_vrm0 = w.vrm0_extras;
            meta
        }))
    }
}
