//! What export needs and the runtime does not use, kept exactly as the file had it (design D8).
//!
//! The runtime [`MaterialSpec`](omuzaki_char_api::MaterialSpec) holds 8-bit sRGB colours, so converting back would
//! nudge every value (linear 0.5 comes back as about 0.502886). [`Retained`] therefore keeps every source material as
//! the file had it (the io-api [`Material`], linear f32 and the whole `MToon` block), and the gather writes that,
//! never a value converted back from the runtime. A person's edit to a material is an explicit field of
//! [`AvatarDef::materials`](crate::def::AvatarDef::materials), applied field by field over the retained material.
//!
//! Everything here serialises (the prepared avatar may be cached in the store), and serde round trips are bit for bit.

use omuzaki_anim_api::spring::JointLimit;
use omuzaki_anim_api::{BoneId, ColliderShape, SpringParams};
use omuzaki_char_api::MaterialName;
use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::{ExtraAttribute, MeshData};
use omuzaki_io_api::{
    FirstPersonFlag, ImageMime, Material, MaterialIx, MorphTarget, Passthrough, Primitive, Texture, VrmMeta,
};
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::AvatarError;

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

/// One primitive of a source mesh: the source material it draws. The runtime meshes it became are in the mesh's
/// [`instances`](RetainedMesh::instances), one per node that draws the mesh.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedPrimitive {
    /// Its material, an index into [`Retained::materials`]; `None` when the file gave it none (glTF's default
    /// material), so export writes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<u32>,
    /// Its geometry and morph targets, kept only for a mesh no node draws: such a mesh has no runtime mesh to read
    /// them from. A drawn primitive's geometry is its runtime mesh's, so this is `None` there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<RetainedGeometry>,
}

impl RetainedPrimitive {
    /// A primitive with no material and no kept geometry.
    pub const fn new() -> Self {
        Self { material: None, geometry: None }
    }

    /// Records its material (builder style).
    #[must_use]
    pub const fn with_material(mut self, material: u32) -> Self {
        self.material = Some(material);
        self
    }

    /// Keeps its geometry, for a mesh no node draws (builder style).
    #[must_use]
    pub fn with_geometry(mut self, geometry: RetainedGeometry) -> Self {
        self.geometry = Some(geometry);
        self
    }
}

/// A named per-vertex attribute beyond the standard set, as a serde mirror of
/// [`ExtraAttribute`] (whose name is a validated slug).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedAttribute {
    /// The attribute's name.
    pub name: String,
    /// Floats per vertex, `1..=4`.
    pub components: u8,
    /// `components` floats for every vertex, vertex after vertex.
    pub data: Vec<f32>,
}

impl RetainedAttribute {
    /// The attribute `name` of `components` floats per vertex.
    pub fn new(name: impl Into<String>, components: u8, data: Vec<f32>) -> Self {
        Self { name: name.into(), components, data }
    }
}

/// One morph target's deltas, a serde mirror of [`MorphTarget`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedMorphTarget {
    /// Position deltas, one per base vertex.
    pub positions: Vec<Vec3>,
    /// Normal deltas, or empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normals: Vec<Vec3>,
}

impl RetainedMorphTarget {
    /// A target with these position and normal deltas.
    pub fn new(positions: Vec<Vec3>, normals: Vec<Vec3>) -> Self {
        Self { positions, normals }
    }
}

/// Everything an io-api [`Primitive`] holds besides its material: the mesh data and the morph targets. Kept for a
/// source mesh no node draws, so export can write it back although the runtime never built a mesh from it.
/// Conversion to and from a [`Primitive`] is exact, bit for bit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedGeometry {
    /// Vertex positions.
    pub positions: Vec<Vec3>,
    /// Normals, one per vertex.
    pub normals: Vec<Vec3>,
    /// Texture coordinates, or empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uvs: Vec<Vec2>,
    /// Linear vertex colours, or empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colours: Vec<[f32; 4]>,
    /// Skinning joints, or empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<[u16; 4]>,
    /// Skinning weights, or empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<[f32; 4]>,
    /// Extra attributes, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<RetainedAttribute>,
    /// The triangle list.
    pub indices: Vec<u32>,
    /// Morph targets, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub morph_targets: Vec<RetainedMorphTarget>,
}

impl RetainedGeometry {
    /// The geometry and morph targets of `primitive`, without its material.
    pub fn from_primitive(primitive: &Primitive) -> Self {
        let m = &primitive.mesh;
        Self {
            positions: m.positions.clone(),
            normals: m.normals.clone(),
            uvs: m.uvs.clone(),
            colours: m.colours.clone(),
            joints: m.joints.clone(),
            weights: m.weights.clone(),
            extra: m
                .extra
                .iter()
                .map(|a| RetainedAttribute::new(a.name.as_str(), a.components, a.data.clone()))
                .collect(),
            indices: m.indices.clone(),
            morph_targets: primitive
                .morph_targets
                .iter()
                .map(|t| RetainedMorphTarget::new(t.positions.clone(), t.normals.clone()))
                .collect(),
        }
    }

    /// The mesh data alone, for [`MeshData::validate`].
    ///
    /// # Errors
    /// [`AvatarError::ExtraAttributeName`] if an extra attribute's name is not a valid slug.
    pub fn to_mesh_data(&self) -> Result<MeshData, AvatarError> {
        let extra = self
            .extra
            .iter()
            .map(|a| {
                let name = Slug::new(a.name.clone()).map_err(|why| AvatarError::extra_attribute_name(&a.name, why))?;
                Ok(ExtraAttribute { name, components: a.components, data: a.data.clone() })
            })
            .collect::<Result<Vec<_>, AvatarError>>()?;
        Ok(MeshData {
            positions: self.positions.clone(),
            normals: self.normals.clone(),
            uvs: self.uvs.clone(),
            colours: self.colours.clone(),
            joints: self.joints.clone(),
            weights: self.weights.clone(),
            extra,
            indices: self.indices.clone(),
        })
    }

    /// The primitive this geometry came from, drawing `material`.
    ///
    /// # Errors
    /// [`AvatarError::ExtraAttributeName`] if an extra attribute's name is not a valid slug.
    pub fn to_primitive(&self, material: Option<MaterialIx>) -> Result<Primitive, AvatarError> {
        let mut primitive = Primitive::new(self.to_mesh_data()?);
        primitive.material = material;
        primitive.morph_targets = self
            .morph_targets
            .iter()
            .map(|t| {
                let mut target = MorphTarget::new(t.positions.clone());
                target.normals.clone_from(&t.normals);
                target
            })
            .collect();
        Ok(primitive)
    }
}

/// One node that draws a source mesh, and the runtime meshes its primitives became there. A mesh drawn by several
/// nodes becomes one set of runtime meshes per node, so each instance deforms and is morphed on its own (a morph bind
/// names its instance's runtime mesh).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedInstance {
    /// The drawing node, an index into [`Retained::nodes`].
    pub node: u32,
    /// The runtime mesh each primitive became at this node, in primitive order: indices into
    /// `ImportedAvatar::meshes`.
    pub meshes: Vec<u16>,
}

impl RetainedInstance {
    /// The instance drawn at `node`, whose primitives became the runtime meshes `meshes`.
    pub fn new(node: u32, meshes: Vec<u16>) -> Self {
        Self { node, meshes }
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

/// One mesh of the file. Each of its primitives becomes a runtime mesh (one material each) at every node that draws
/// it, and they share its morph-target names: glTF names targets per mesh, not per primitive. A mesh no node draws has
/// no instance and no runtime mesh, so its primitives carry their geometry ([`RetainedPrimitive::geometry`]) instead.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedMesh {
    /// The mesh's name in the file.
    pub name: String,
    /// Its primitives, in file order, with their materials.
    pub primitives: Vec<RetainedPrimitive>,
    /// The nodes that draw it, in node order, each with the runtime meshes its primitives became there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<RetainedInstance>,
    /// Its morph-target names as the file has them (`extras.targetNames`, `Fcl_ALL_A`), one per target; empty when
    /// the file names none. The runtime names are slugs (`SparseMorph::name`), so these are what export writes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub morph_target_names: Vec<String>,
}

impl RetainedMesh {
    /// The mesh `name` with `primitives`, drawn by no node yet, with no target names.
    pub fn new(name: impl Into<String>, primitives: Vec<RetainedPrimitive>) -> Self {
        Self { name: name.into(), primitives, instances: Vec::new(), morph_target_names: Vec::new() }
    }

    /// Adds the instance drawn at `node`, whose primitives became the runtime meshes `meshes` (builder style).
    #[must_use]
    pub fn with_instance(mut self, node: u32, meshes: Vec<u16>) -> Self {
        self.instances.push(RetainedInstance::new(node, meshes));
        self
    }

    /// Sets its morph-target names (builder style).
    #[must_use]
    pub fn with_morph_target_names(mut self, names: Vec<String>) -> Self {
        self.morph_target_names = names;
        self
    }

    /// The instance drawn at `node`, if that node draws this mesh.
    pub fn instance(&self, node: u32) -> Option<&RetainedInstance> {
        self.instances.iter().find(|i| i.node == node)
    }
}

/// One spring collider of the file (`VRMC_springBone.colliders[*]`), in the file's collider list: groups name it by
/// index, so one collider can be in several groups, or in none.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedCollider {
    /// The node it moves with, an index into [`Retained::nodes`].
    pub node: u32,
    /// Its shape in that node's space, as the file has it.
    pub shape: ColliderShape,
}

impl RetainedCollider {
    /// A collider of `shape` on `node`.
    pub const fn new(node: u32, shape: ColliderShape) -> Self {
        Self { node, shape }
    }
}

/// One collider group of the file (`VRMC_springBone.colliderGroups[*]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedColliderGroup {
    /// Its name as the file has it: it may be empty, or the same as another group's.
    pub name: String,
    /// Its colliders, as indices into [`RetainedSprings::colliders`], in file order.
    pub colliders: Vec<u32>,
    /// The name of the runtime group made from it (`SpringSet::groups`, unique), or `None` when the importer
    /// made none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

impl RetainedColliderGroup {
    /// The group `name` over `colliders`, made into no runtime group yet.
    pub fn new(name: impl Into<String>, colliders: Vec<u32>) -> Self {
        Self { name: name.into(), colliders, runtime: None }
    }

    /// Records the runtime group made from it (builder style).
    #[must_use]
    pub fn with_runtime(mut self, runtime: impl Into<String>) -> Self {
        self.runtime = Some(runtime.into());
        self
    }
}

/// One joint of a spring chain of the file.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedSpringJoint {
    /// The node that swings, an index into [`Retained::nodes`].
    pub node: u32,
    /// Its parameters as the file has them.
    pub params: SpringParams,
    /// Its angular limit (`VRMC_springBone_limit`), if the file gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<JointLimit>,
}

impl RetainedSpringJoint {
    /// The joint at `node` with `params` and no limit.
    pub const fn new(node: u32, params: SpringParams) -> Self {
        Self { node, params, limit: None }
    }

    /// Sets its limit (builder style).
    #[must_use]
    pub const fn with_limit(mut self, limit: JointLimit) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// One spring chain of the file (`VRMC_springBone.springs[*]`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedSpringChain {
    /// Its name as the file has it: it may be empty, or the same as another chain's.
    pub name: String,
    /// Its joints, root first.
    pub joints: Vec<RetainedSpringJoint>,
    /// The collider groups it collides with, as indices into [`RetainedSprings::groups`], in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<u32>,
    /// The node whose space it is simulated in, an index into [`Retained::nodes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center: Option<u32>,
    /// The name of the runtime chain made from it (`SpringSet::chains`, unique), or `None` when the importer made
    /// none (a chain the solver cannot run, reported at import).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

impl RetainedSpringChain {
    /// The chain `name` over `joints`, with no groups and no centre, made into no runtime chain yet.
    pub fn new(name: impl Into<String>, joints: Vec<RetainedSpringJoint>) -> Self {
        Self { name: name.into(), joints, groups: Vec::new(), center: None, runtime: None }
    }

    /// Sets the collider groups it uses (builder style).
    #[must_use]
    pub fn with_groups(mut self, groups: Vec<u32>) -> Self {
        self.groups = groups;
        self
    }

    /// Sets its centre node (builder style).
    #[must_use]
    pub fn with_center(mut self, center: u32) -> Self {
        self.center = Some(center);
        self
    }

    /// Records the runtime chain made from it (builder style).
    #[must_use]
    pub fn with_runtime(mut self, runtime: impl Into<String>) -> Self {
        self.runtime = Some(runtime.into());
        self
    }
}

/// The file's spring records, by index as the file has them (`VRMC_springBone`).
///
/// The runtime [`SpringSet`](omuzaki_anim_api::SpringSet) is what the solver runs: colliders inside uniquely named
/// groups, chains naming groups by name. It cannot say that one collider is shared by two groups, that a collider is
/// in no group, or that two groups or chains share a name, so the file's own records are kept here, exactly, and
/// export writes them (D8), as it writes retained materials. Each group and chain names the runtime entry made from
/// it, so the report and a later edit can find it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RetainedSprings {
    /// Every collider, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colliders: Vec<RetainedCollider>,
    /// Every collider group, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<RetainedColliderGroup>,
    /// Every chain, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chains: Vec<RetainedSpringChain>,
}

impl RetainedSprings {
    /// No spring records.
    pub fn new() -> Self {
        Self::default()
    }

    /// True if the file had no colliders, groups or chains.
    pub fn is_empty(&self) -> bool {
        self.colliders.is_empty() && self.groups.is_empty() && self.chains.is_empty()
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
    /// The file's meshes, in file order: which runtime meshes each one's primitives became at each node that draws
    /// it, and its morph-target names, so export rebuilds each mesh once, with its primitives and its one
    /// `extras.targetNames` table. Empty for an avatar that has none to keep.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meshes: Vec<RetainedMesh>,
    /// The file's skins, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skins: Vec<RetainedSkin>,
    /// The file's spring colliders, groups and chains, by index as the file has them.
    #[serde(default, skip_serializing_if = "RetainedSprings::is_empty")]
    pub springs: RetainedSprings,
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
