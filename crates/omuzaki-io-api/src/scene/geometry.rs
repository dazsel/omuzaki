//! Meshes, morph targets and skins.
//!
//! A primitive's vertex data is a [`MeshData`], the shared CPU mesh type (positions, normals, UVs, vertex colours,
//! four joints and weights, a triangle list) that the renderer, the character kit and the importers all speak, so
//! turning an imported primitive into something drawable is a move, not a conversion. Importers resolve glTF buffer
//! views, strides, normalisation and sparse accessors into it; exporters re-pack. The fields glTF lets a file leave
//! out are filled in by the importer so every primitive satisfies the one rule the renderer relies on:
//! - **normals** are always present; a file without `NORMAL` gets flat normals ([`Primitive::generate_flat_normals`]),
//!   as glTF 2.0 requires of clients, plus an info finding;
//! - **indices** are always present; an unindexed primitive gets `0..n`.
//!
//! Only triangle lists are representable: other primitive modes are converted or rejected by the importer
//! (glTF 2.0, "Topology and Connectivity").

use omuzaki_core::glam::{Mat4, Vec3};

use super::{MaterialIx, NodeIx};

pub use omuzaki_core::mesh::{ExtraAttribute, MeshData};

/// A mesh: one or more primitives that share morph targets.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Mesh {
    /// Display name.
    pub name: String,
    /// The draw calls. Every primitive that defines morph targets defines the same number (glTF 2.0 §"Morph
    /// Targets": "all mesh primitives of that mesh that define morph targets MUST have the same number").
    pub primitives: Vec<Primitive>,
    /// One name per morph target, or empty. glTF has no name field for targets; the de-facto convention, which the
    /// VRM 1.0 spec recommends (`VRMC_vrm-1.0/README.md`, "`meshes[*].extras.targetNames`"), is the mesh's
    /// `extras.targetNames` array. The count equals [`Mesh::morph_target_count`] when present.
    pub morph_target_names: Vec<String>,
}

impl Mesh {
    /// A named mesh with no primitives.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }

    /// The number of morph targets: that of the first primitive that has any.
    pub fn morph_target_count(&self) -> usize {
        self.primitives.iter().map(|p| p.morph_targets.len()).find(|&n| n > 0).unwrap_or(0)
    }
}

/// One draw call: a triangle list with one material, and its morph targets.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Primitive {
    /// The geometry. `JOINTS_0`/`WEIGHTS_0` are the only skinning sets kept: `JOINTS_1` and up are folded in by the
    /// importer (largest four, renormalised) with a warning. `TANGENT` is not stored: VRM 1.0 says to recompute it
    /// with `MikkTSpace` on import and not to export it.
    pub mesh: MeshData,
    /// The material, or `None` for glTF's default material.
    pub material: Option<MaterialIx>,
    /// Morph targets as deltas from the base vertices. The shared mesh type has none (morphing is a file-format and
    /// expression-bake concern, spec §19 FE-09), so they live here.
    pub morph_targets: Vec<MorphTarget>,
}

impl Primitive {
    /// A primitive with this geometry, no material and no morph targets.
    pub fn new(mesh: MeshData) -> Self {
        Self { mesh, ..Self::default() }
    }

    /// Gives the primitive flat normals, as glTF 2.0 requires of clients when a file has none: every triangle corner
    /// becomes its own vertex (attributes and morph target deltas are copied along) and indices become `0..n`.
    /// Returns false, leaving everything untouched, if the mesh is inconsistent (an index past the last vertex, or an
    /// attribute whose length is neither 0 nor the vertex count); [`IoScene::check`](crate::IoScene::check) reports
    /// those. A trailing partial triangle is dropped. Every importer needs this, so it lives here once.
    pub fn generate_flat_normals(&mut self) -> bool {
        let m = &self.mesh;
        let n = m.positions.len();
        let consistent = [m.normals.len(), m.uvs.len(), m.colours.len(), m.joints.len(), m.weights.len()]
            .into_iter()
            .chain(self.morph_targets.iter().flat_map(|t| [t.positions.len(), t.normals.len()]))
            .all(|len| len == 0 || len == n);
        let sequential: Vec<u32>;
        let source = if m.indices.is_empty() {
            sequential = (0..u32::try_from(n).unwrap_or(u32::MAX)).collect();
            &sequential
        } else {
            &m.indices
        };
        let corners: Vec<u32> = source.as_chunks::<3>().0.iter().flatten().copied().collect();
        if !consistent || corners.iter().any(|&i| i as usize >= n) {
            return false;
        }
        let m = &mut self.mesh;
        let normals = corners
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|t| {
                let p = |k: usize| m.positions[t[k] as usize];
                let normal = (p(1) - p(0)).cross(p(2) - p(0)).try_normalize().unwrap_or(Vec3::Z);
                [normal; 3]
            })
            .collect();
        gather(&mut m.positions, &corners);
        gather(&mut m.uvs, &corners);
        gather(&mut m.colours, &corners);
        gather(&mut m.joints, &corners);
        gather(&mut m.weights, &corners);
        for t in &mut self.morph_targets {
            gather(&mut t.positions, &corners);
            gather(&mut t.normals, &corners);
        }
        m.normals = normals;
        m.indices = (0..u32::try_from(corners.len()).unwrap_or(u32::MAX)).collect();
        true
    }
}

/// Replaces `items` with `items[order[0]], items[order[1]], …`; an absent (empty) attribute stays absent. The caller has
/// checked every index is in range.
fn gather<T: Copy>(items: &mut Vec<T>, order: &[u32]) {
    if !items.is_empty() {
        *items = order.iter().filter_map(|&i| items.get(i as usize).copied()).collect();
    }
}

/// A morph target's deltas. `TANGENT` deltas are not stored (VRM 1.0: "morphTarget tangent is not recommended").
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct MorphTarget {
    /// Position deltas, one per base vertex.
    pub positions: Vec<Vec3>,
    /// Normal deltas, or empty.
    pub normals: Vec<Vec3>,
}

impl MorphTarget {
    /// A target with these position deltas and no normal deltas.
    pub fn new(positions: Vec<Vec3>) -> Self {
        Self { positions, normals: Vec::new() }
    }
}

/// A skin: the joint nodes that deform a mesh and their inverse bind matrices.
///
/// The inverse bind matrices are **data, not derived** from the nodes' rest transforms. A model sculpted in an A-pose
/// but rigged on a T-pose skeleton has bind matrices that differ from the rest pose ("Bind the mesh in A,
/// rest the skeleton in T"), and an exporter that recomputed them would flatten the sculpt.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Skin {
    /// Display name.
    pub name: String,
    /// The joint nodes, in the order `JOINTS_0` indexes them.
    pub joints: Vec<NodeIx>,
    /// One matrix per joint (mesh space to joint space at bind time).
    pub inverse_bind: Vec<Mat4>,
    /// The skeleton root, when the file names one.
    pub skeleton: Option<NodeIx>,
}

impl Skin {
    /// A skin over `joints` with their inverse bind matrices (one per joint) and no named skeleton root.
    pub fn new(name: impl Into<String>, joints: Vec<NodeIx>, inverse_bind: Vec<Mat4>) -> Self {
        Self { name: name.into(), joints, inverse_bind, skeleton: None }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec2;

    use super::*;

    fn quad() -> Primitive {
        // Two counter-clockwise triangles in the XY plane sharing an edge; the morph target lifts only vertex 3.
        let mesh = MeshData {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::new(0.0, 1.0, 0.0), Vec3::new(1.0, 1.0, 0.0)],
            uvs: vec![Vec2::ZERO, Vec2::X, Vec2::Y, Vec2::ONE],
            indices: vec![0, 1, 2, 2, 1, 3],
            ..MeshData::default()
        };
        let mut p = Primitive::new(mesh);
        p.morph_targets = vec![MorphTarget::new(vec![Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::Z])];
        p
    }

    #[test]
    fn flat_normals_unshare_vertices_and_carry_every_attribute_along() {
        let mut p = quad();
        assert!(p.generate_flat_normals());
        let m = &p.mesh;
        assert_eq!((m.positions.len(), m.normals.len(), m.uvs.len(), m.indices.len()), (6, 6, 6, 6));
        assert_eq!(m.indices, [0, 1, 2, 3, 4, 5]);
        assert!(m.normals.iter().all(|n| (*n - Vec3::Z).length() < 1e-6), "counter-clockwise in XY faces +Z");
        assert_eq!(m.positions[5], Vec3::new(1.0, 1.0, 0.0), "the last corner is the shared vertex 3");
        assert_eq!(m.uvs[5], Vec2::ONE);
        // The morph delta of old vertex 3 now sits on the one corner that used it.
        assert_eq!(p.morph_targets[0].positions, [Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::Z]);
    }

    #[test]
    fn an_unindexed_primitive_is_handled_and_a_broken_one_is_left_alone() {
        let mut flat =
            Primitive::new(MeshData { positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y], ..MeshData::default() });
        assert!(flat.generate_flat_normals());
        assert_eq!((flat.mesh.indices.as_slice(), flat.mesh.normals.len()), (&[0, 1, 2][..], 3));

        let mut broken = quad();
        broken.mesh.indices[0] = 9;
        let before = broken.clone();
        assert!(!broken.generate_flat_normals());
        assert_eq!(broken, before, "nothing changes when the mesh is inconsistent");
        let mut short = quad();
        short.mesh.uvs.pop();
        assert!(!short.generate_flat_normals());
    }
}
