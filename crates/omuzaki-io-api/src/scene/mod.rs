//! The intermediate scene: what a glTF or VRM file *says*, in memory, in one canonical form.
//!
//! Every importer produces an [`IoScene`] and every exporter consumes one, so N formats cost N importers plus N
//! exporters, not N×N converters (spec §27). It is deliberately shaped like glTF 2.0 (arenas of nodes, meshes,
//! skins, materials, textures and images, linked by typed indices) because every format we handle is glTF-shaped,
//! and because lossless round trips need the file's own vocabulary, not the runtime's.
//!
//! **What is shared with the runtime, and what is not.** The CPU mesh is the shared [`MeshData`] (the renderer, the
//! character kit and the importers all speak it), and colours are `glam` vectors of linear factors, exactly what
//! glTF stores. The *scene* is not the character or render model: those are runtime models, tuned for editing and for
//! 1.5 ms frames, and a file model must also carry things the runtime has no use for (a thumbnail,
//! `thirdPartyLicenses`, morph-target names, spring-bone collider groups, the `MToon` fields the renderer doesn't
//! model) and must not change when the renderer does. Conversion in either direction is a pure function owned by the
//! op layer (`omuzaki-io-ops`), the only crate that depends on both sides; see `docs/design/io-api.md`.
//!
//! **Canonical space.** An `IoScene` is always in glTF 2.0 / VRM 1.0 space: right-handed, +Y up, the model's front
//! facing +Z, metres, radians, unit quaternions, linear colour factors.
//! VRM 0.x files face −Z, so the 0.x importer rotates them 180° about Y on the way in (`x, z → −x, −z` for
//! translations and vertex data) and the 0.x exporter rotates back. Nothing downstream ever sees a 0.x orientation.
//!
//! **Invariants** are written down once, in [`IoScene::check`], and enforced at the boundary by
//! [`run_import`](crate::run_import) and [`run_export`](crate::run_export), so a consumer can index without
//! bounds worries.

mod avatar;
mod geometry;
mod humanoid;
mod material;
mod passthrough;

use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_store_api::Provenance;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use avatar::{
    AimAxis, Avatar, Collider, ColliderGroup, ColliderShape, ColorBind, ColorBindKind, ConstraintKind, CustomName,
    CustomNameError, Expression, ExpressionName, FirstPerson, FirstPersonFlag, JointLimit, LookAt, LookAtKind,
    MeshAnnotation, MorphBind, NodeConstraint, Override, PresetExpression, RangeMap, RollAxis, SpringBones,
    SpringChain, SpringJoint, UvBind,
};
pub use geometry::{ExtraAttribute, Mesh, MeshData, MorphTarget, Primitive, Skin};
pub use humanoid::{BoneSpec, HUMAN_BONES, HumanBoneName, Humanoid, UnknownBone};
pub use material::{
    AlphaMode, Blob, EncodedImage, Filter, ImageMime, Material, NormalTexture, OutlineMode, Pbr, Sampler, Shading,
    ShiftTexture, TexRef, Texture, Toon, UvTransform, Wrap,
};
pub use passthrough::Passthrough;

use crate::Finding;
use crate::meta::VrmMeta;

/// Declares a typed index into one of the scene's arenas, so a `MeshIx` can't be used where a `NodeIx` is wanted.
macro_rules! index_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl $name {
            /// The index as a `usize`, for slicing.
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

index_type!(
    /// An index into [`IoScene::nodes`].
    NodeIx
);
index_type!(
    /// An index into [`IoScene::meshes`].
    MeshIx
);
index_type!(
    /// An index into [`IoScene::skins`].
    SkinIx
);
index_type!(
    /// An index into [`IoScene::materials`].
    MaterialIx
);
index_type!(
    /// An index into [`IoScene::textures`].
    TextureIx
);
index_type!(
    /// An index into [`IoScene::images`].
    ImageIx
);

/// Translation, rotation and scale. glTF's `matrix` form is decomposed on import (a non-decomposable matrix is an
/// import warning), so exporters only ever write TRS.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Transform {
    /// Metres, relative to the parent.
    pub translation: Vec3,
    /// A unit quaternion.
    pub rotation: Quat,
    /// Per-axis scale. Humanoid bones must have positive components (VRM 1.0 `humanoid.md`, "Forbidden Transforms").
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self { translation: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::ONE }
    }
}

/// One node of the hierarchy. It may carry a mesh (optionally skinned) and has children.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Node {
    /// Display name; not unique.
    pub name: String,
    /// Local transform.
    pub transform: Transform,
    /// Children, in order. A node has at most one parent and the graph has no cycles.
    pub children: Vec<NodeIx>,
    /// The mesh drawn at this node.
    pub mesh: Option<MeshIx>,
    /// The skin that deforms that mesh.
    pub skin: Option<SkinIx>,
    /// Unknown extensions and extras on this node, carried through unchanged.
    pub passthrough: Passthrough,
}

impl Node {
    /// A named node at the origin.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }

    /// Places the node at `translation` (builder style).
    #[must_use]
    pub fn at(mut self, translation: Vec3) -> Self {
        self.transform.translation = translation;
        self
    }
}

/// A whole model as a file describes it. Build one with [`IoScene::default`] and the `add_*` methods; the fields are
/// public so importers can fill them directly, and `#[non_exhaustive]` so later additions don't break them.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct IoScene {
    /// Every node.
    pub nodes: Vec<Node>,
    /// The nodes with no parent that make up the default scene (glTF `scenes[scene].nodes`).
    pub roots: Vec<NodeIx>,
    /// Every mesh.
    pub meshes: Vec<Mesh>,
    /// Every skin.
    pub skins: Vec<Skin>,
    /// Every material.
    pub materials: Vec<Material>,
    /// Every texture (an image plus a sampler).
    pub textures: Vec<Texture>,
    /// Every image, as the original PNG or JPEG bytes (never re-encoded by the contract).
    pub images: Vec<EncodedImage>,
    /// What the file says about the model: its name, authors and licence terms, in `VRMC_vrm.meta` vocabulary.
    pub meta: VrmMeta,
    /// What *Omuzaki* has recorded about the rights: the §7 provenance record. `run_import` fills it from `meta` with
    /// the conservative mapping in [`VrmMeta::imported_provenance`]; a user can then correct it. Exporters refuse
    /// a scene without one (`rights.provenance_missing`).
    pub provenance: Option<Provenance>,
    /// The humanoid avatar data (VRM). `None` for a plain glTF.
    pub avatar: Option<Avatar>,
    /// The `asset.generator` of the source file, kept for diagnostics only; exporters write their own.
    pub source_generator: Option<String>,
    /// Unknown root-level extensions and extras, carried through unchanged.
    pub passthrough: Passthrough,
}

/// Pushes `item` and returns its index. glTF indices are `u32`; a scene with more than 4 billion items can't exist
/// in memory, so the saturating conversion only documents that, it never fires.
fn push<T>(items: &mut Vec<T>, item: T) -> u32 {
    let ix = u32::try_from(items.len()).unwrap_or(u32::MAX);
    items.push(item);
    ix
}

impl IoScene {
    /// An empty scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node and returns its index.
    pub fn add_node(&mut self, node: Node) -> NodeIx {
        NodeIx(push(&mut self.nodes, node))
    }

    /// Adds a mesh and returns its index.
    pub fn add_mesh(&mut self, mesh: Mesh) -> MeshIx {
        MeshIx(push(&mut self.meshes, mesh))
    }

    /// Adds a skin and returns its index.
    pub fn add_skin(&mut self, skin: Skin) -> SkinIx {
        SkinIx(push(&mut self.skins, skin))
    }

    /// Adds a material and returns its index.
    pub fn add_material(&mut self, material: Material) -> MaterialIx {
        MaterialIx(push(&mut self.materials, material))
    }

    /// Adds a texture and returns its index.
    pub fn add_texture(&mut self, texture: Texture) -> TextureIx {
        TextureIx(push(&mut self.textures, texture))
    }

    /// Adds an image and returns its index.
    pub fn add_image(&mut self, image: EncodedImage) -> ImageIx {
        ImageIx(push(&mut self.images, image))
    }

    /// Makes `child` a child of `parent`. Use [`IoScene::check`] to verify the result is a forest.
    pub fn attach(&mut self, parent: NodeIx, child: NodeIx) {
        if let Some(p) = self.nodes.get_mut(parent.index()) {
            p.children.push(child);
        }
    }

    /// The node that lists `node` as a child. Linear in the number of nodes: importers and checks that need many
    /// lookups build their own parent table once.
    pub fn parent_of(&self, node: NodeIx) -> Option<NodeIx> {
        let found = self.nodes.iter().position(|n| n.children.contains(&node))?;
        Some(NodeIx(u32::try_from(found).unwrap_or(u32::MAX)))
    }

    /// An info finding (`gltf.passthrough`) listing every unknown extension and extras blob being carried, or `None`
    /// if there are none. Reported on import and again in the export preflight.
    pub fn passthrough_finding(&self) -> Option<Finding> {
        let mut parts = Vec::new();
        let mut add = |place: String, p: &Passthrough| {
            if !p.is_empty() {
                parts.push(format!("{place}: {}", p.describe()));
            }
        };
        add("scene".to_owned(), &self.passthrough);
        for (i, n) in self.nodes.iter().enumerate() {
            add(format!("node {i}"), &n.passthrough);
        }
        for (i, m) in self.materials.iter().enumerate() {
            add(format!("material {i}"), &m.passthrough);
        }
        if parts.is_empty() {
            return None;
        }
        let msg = format!("kept as raw JSON and written back unchanged, not interpreted: {}", parts.join("; "));
        Some(Finding::info("gltf.passthrough", msg).at("passthrough").hint("indices inside it are not remapped"))
    }

    /// A one-screen summary for op output, so an agent can see what an import produced without the geometry.
    pub fn summary(&self) -> SceneSummary {
        let prims = || self.meshes.iter().flat_map(|m| m.primitives.iter());
        let count = |n: usize| n as u64;
        let triangles: usize = prims().map(|p| p.mesh.triangle_count()).sum();
        SceneSummary {
            nodes: count(self.nodes.len()),
            meshes: count(self.meshes.len()),
            primitives: count(prims().count()),
            vertices: count(prims().map(|p| p.mesh.positions.len()).sum()),
            triangles: count(triangles),
            morph_targets: count(self.meshes.iter().map(Mesh::morph_target_count).sum()),
            skins: count(self.skins.len()),
            materials: count(self.materials.len()),
            toon_materials: count(self.materials.iter().filter(|m| matches!(m.shading, Shading::Toon(_))).count()),
            textures: count(self.textures.len()),
            images: count(self.images.len()),
            image_bytes: count(self.images.iter().map(|i| i.bytes.len()).sum()),
            humanoid_bones: count(self.avatar.as_ref().map_or(0, |a| a.humanoid.bones.len())),
            expressions: count(self.avatar.as_ref().map_or(0, |a| a.expressions.len())),
            spring_chains: count(self.avatar.as_ref().map_or(0, |a| a.spring_bones.chains.len())),
            colliders: count(self.avatar.as_ref().map_or(0, |a| a.spring_bones.colliders.len())),
        }
    }
}

/// Counts describing an [`IoScene`], returned by import ops and `io.inspect`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SceneSummary {
    /// Nodes.
    pub nodes: u64,
    /// Meshes.
    pub meshes: u64,
    /// Mesh primitives (draw calls).
    pub primitives: u64,
    /// Vertices across all primitives.
    pub vertices: u64,
    /// Triangles across all primitives.
    pub triangles: u64,
    /// Morph targets across all meshes (each mesh counted once).
    pub morph_targets: u64,
    /// Skins.
    pub skins: u64,
    /// Materials.
    pub materials: u64,
    /// Materials that use `MToon` shading.
    pub toon_materials: u64,
    /// Textures.
    pub textures: u64,
    /// Images.
    pub images: u64,
    /// Total bytes of image data.
    pub image_bytes: u64,
    /// Humanoid bones assigned.
    pub humanoid_bones: u64,
    /// Expressions (preset and custom).
    pub expressions: u64,
    /// Spring-bone chains.
    pub spring_chains: u64,
    /// Spring-bone colliders.
    pub colliders: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_returns_sequential_typed_indices_and_attach_links() {
        let mut s = IoScene::default();
        let a = s.add_node(Node::new("a"));
        let b = s.add_node(Node::new("b").at(Vec3::Y));
        s.attach(a, b);
        assert_eq!((a, b), (NodeIx(0), NodeIx(1)));
        assert_eq!(s.parent_of(b), Some(a));
        assert_eq!(s.parent_of(a), None);
        assert_eq!(s.nodes[1].transform.translation, Vec3::Y);
    }

    #[test]
    fn summary_counts_vertices_and_triangles_across_primitives() {
        let mut s = IoScene::default();
        let quad = MeshData {
            positions: vec![Vec3::ZERO; 4],
            normals: vec![Vec3::Z; 4],
            indices: vec![0, 1, 2, 2, 1, 3],
            ..MeshData::default()
        };
        let tri = MeshData {
            positions: vec![Vec3::ZERO; 3],
            normals: vec![Vec3::Z; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        };
        let mut mesh = Mesh::new("m");
        mesh.primitives = vec![Primitive::new(quad), Primitive::new(tri)];
        s.add_mesh(mesh);
        let sum = s.summary();
        assert_eq!((sum.primitives, sum.vertices, sum.triangles), (2, 7, 3));
    }
}
