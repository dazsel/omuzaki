//! Dynamic meshes and sparse morph targets: the creation-time options of [`Renderer::create_mesh_with`].
//!
//! **Why sparse.** A face has dozens of blend shapes and each moves a small part of the head, so storing only the
//! moved vertices keeps the upload and the per-frame blend proportional to what actually changes. The dense layout
//! importers produce (`omuzaki-io-api`'s `MorphTarget`: one delta per base vertex) converts with
//! [`MorphSet::from_dense`].
//!
//! **What a morphed vertex is.** For an instance with weights `w`, the renderer computes, per vertex,
//! `position = base + Σ wᵢ·Δpositionᵢ` and `normal = normalize(base_normal + Σ wᵢ·Δnormalᵢ)`, in target order, and
//! only then skins the result (as glTF does). Weights are not clamped.
//!
//! [`Renderer::create_mesh_with`]: crate::Renderer::create_mesh_with

use omuzaki_core::glam::Vec3;
use omuzaki_core::id::Slug;

use crate::RenderError;

/// How a mesh's vertices may change after creation.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum MeshUsage {
    /// Uploaded once; [`Renderer::update_vertices`](crate::Renderer::update_vertices) is an error.
    #[default]
    Static,
    /// Positions and normals may be replaced every frame (cloth, soft bodies, CPU-deformed hair).
    Dynamic,
}

/// One morph target stored sparsely: only the vertices it moves.
#[derive(Clone, Debug, PartialEq)]
pub struct SparseMorph {
    /// The target's name (a blend-shape or expression name).
    pub name: Slug,
    /// The moved vertices, strictly ascending, each below the mesh's vertex count.
    pub indices: Vec<u32>,
    /// A position delta for each entry of `indices`, metres.
    pub positions: Vec<Vec3>,
    /// A normal delta for each entry of `indices`, or empty if the target leaves normals alone.
    pub normals: Vec<Vec3>,
}

impl SparseMorph {
    fn validate(&self, vertex_count: usize) -> Result<(), RenderError> {
        let bad = |reason: String| Err(RenderError::invalid("morph target", format!("{:?}: {reason}", self.name)));
        let n = self.indices.len();
        if self.positions.len() != n {
            return bad(format!("{} position deltas for {n} indices", self.positions.len()));
        }
        if !self.normals.is_empty() && self.normals.len() != n {
            return bad(format!("{} normal deltas for {n} indices (or none)", self.normals.len()));
        }
        if let Some(&[a, b]) = self.indices.windows(2).find(|w| w[0] >= w[1]) {
            return bad(format!("indices must be strictly ascending, but {a} is followed by {b}"));
        }
        if let Some(&last) = self.indices.last().filter(|&&i| i as usize >= vertex_count) {
            return bad(format!("index {last} is past the last vertex ({})", vertex_count.saturating_sub(1)));
        }
        if self.positions.iter().chain(&self.normals).any(|d| !d.is_finite()) {
            return bad("deltas must be finite".into());
        }
        Ok(())
    }
}

/// The sparse morph targets of one mesh, uploaded once at creation. Build one with [`MorphSet::from_dense`] (or
/// [`MorphSet::from_targets`]); a renderer re-checks it with [`validate`](MorphSet::validate).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MorphSet {
    targets: Vec<SparseMorph>,
}

impl MorphSet {
    /// A set from ready-made sparse targets, which must pass [`validate`](MorphSet::validate) for `vertex_count`.
    ///
    /// # Errors
    /// [`RenderError::Invalid`], as [`validate`](MorphSet::validate).
    pub fn from_targets(vertex_count: usize, targets: Vec<SparseMorph>) -> Result<Self, RenderError> {
        let set = Self { targets };
        set.validate(vertex_count)?;
        Ok(set)
    }

    /// From dense per-vertex deltas (`omuzaki-io-api`'s `MorphTarget` layout: one position delta per base vertex, and
    /// either no normal deltas or one per vertex), dropping every vertex whose position and normal deltas both have
    /// length `<= epsilon`. Pass `0.0` to drop only exact zeros.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a negative or non-finite `epsilon`, a position delta count other than
    /// `vertex_count`, a normal delta count that is neither 0 nor `vertex_count`, or a non-finite delta.
    pub fn from_dense(
        vertex_count: usize,
        targets: &[(Slug, &[Vec3], &[Vec3])],
        epsilon: f32,
    ) -> Result<Self, RenderError> {
        if !(epsilon >= 0.0 && epsilon.is_finite()) {
            return Err(RenderError::invalid("morph epsilon", format!("{epsilon} must be finite and >= 0")));
        }
        let mut sparse = Vec::with_capacity(targets.len());
        for (name, positions, normals) in targets {
            let bad = |reason: String| Err(RenderError::invalid("morph target", format!("{name:?}: {reason}")));
            if positions.len() != vertex_count {
                return bad(format!("{} position deltas for {vertex_count} vertices", positions.len()));
            }
            if !normals.is_empty() && normals.len() != vertex_count {
                return bad(format!("{} normal deltas for {vertex_count} vertices (or none)", normals.len()));
            }
            if positions.iter().chain(*normals).any(|d| !d.is_finite()) {
                return bad("deltas must be finite".into());
            }
            let mut morph =
                SparseMorph { name: name.clone(), indices: Vec::new(), positions: Vec::new(), normals: Vec::new() };
            for (i, &p) in positions.iter().enumerate() {
                let n = normals.get(i).copied().unwrap_or(Vec3::ZERO);
                if p.length() > epsilon || n.length() > epsilon {
                    // `vertex_count` is a mesh's vertex count, which indices (u32) already bound.
                    morph
                        .indices
                        .push(u32::try_from(i).map_err(|_| RenderError::invalid("morph target", "too many vertices"))?);
                    morph.positions.push(p);
                    if !normals.is_empty() {
                        morph.normals.push(n);
                    }
                }
            }
            sparse.push(morph);
        }
        Ok(Self { targets: sparse })
    }

    /// The targets, in weight order: weight `i` of an instance's [`MorphRange`](crate::MorphRange) drives target `i`.
    pub fn targets(&self) -> &[SparseMorph] {
        &self.targets
    }

    /// How many targets.
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// True if there are no targets.
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Checks every rule a renderer relies on for a mesh of `vertex_count` vertices.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] naming the first broken rule: unsorted or repeated indices, an index past the last
    /// vertex, mismatched delta counts (normals may be empty) or a non-finite delta.
    pub fn validate(&self, vertex_count: usize) -> Result<(), RenderError> {
        self.targets.iter().try_for_each(|t| t.validate(vertex_count))
    }
}

/// What a mesh is created with, beyond its geometry. The default is a plain static mesh, which every renderer supports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshOptions {
    /// Whether the vertices may be replaced later.
    pub usage: MeshUsage,
    /// Morph targets, if the mesh has blend shapes.
    pub morphs: Option<MorphSet>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> Slug {
        Slug::new(s).unwrap()
    }

    fn sparse(indices: Vec<u32>, positions: Vec<Vec3>, normals: Vec<Vec3>) -> SparseMorph {
        SparseMorph { name: name("smile"), indices, positions, normals }
    }

    fn dense_one(positions: &[Vec3], normals: &[Vec3], epsilon: f32) -> Result<MorphSet, RenderError> {
        MorphSet::from_dense(positions.len(), &[(name("smile"), positions, normals)], epsilon)
    }

    #[test]
    fn from_dense_keeps_only_the_moved_vertices() {
        let p = [Vec3::ZERO, Vec3::X, Vec3::ZERO, Vec3::new(0.0, 0.0, -2.0)];
        let set = dense_one(&p, &[], 0.0).unwrap();
        let t = &set.targets()[0];
        assert_eq!((set.len(), set.is_empty()), (1, false));
        assert_eq!(t.indices, vec![1, 3]);
        assert_eq!(t.positions, vec![Vec3::X, Vec3::new(0.0, 0.0, -2.0)]);
        assert_eq!(t.normals, Vec::<Vec3>::new());
        assert_eq!(set.validate(4), Ok(()));
    }

    #[test]
    fn epsilon_drops_small_deltas_and_is_inclusive() {
        let p = [Vec3::new(0.001, 0.0, 0.0), Vec3::new(0.01, 0.0, 0.0), Vec3::new(0.02, 0.0, 0.0)];
        let set = dense_one(&p, &[], 0.01).unwrap();
        assert_eq!(set.targets()[0].indices, vec![2], "|d| <= epsilon is dropped, including equal");
        assert_eq!(dense_one(&p, &[], 0.0).unwrap().targets()[0].indices, vec![0, 1, 2]);
    }

    #[test]
    fn a_vertex_with_only_a_normal_delta_is_kept_with_a_zero_position_delta() {
        let p = [Vec3::ZERO, Vec3::ZERO];
        let n = [Vec3::ZERO, Vec3::Y];
        let t = &dense_one(&p, &n, 0.0).unwrap().targets()[0].clone();
        assert_eq!(
            (t.indices.clone(), t.positions.clone(), t.normals.clone()),
            (vec![1], vec![Vec3::ZERO], vec![Vec3::Y])
        );
    }

    #[test]
    fn an_empty_target_is_valid_and_keeps_its_slot() {
        let set = dense_one(&[Vec3::ZERO; 3], &[], 0.0).unwrap();
        assert_eq!((set.len(), set.targets()[0].indices.len()), (1, 0), "weights still line up with targets");
    }

    #[test]
    fn from_dense_rejects_bad_input() {
        let two = [Vec3::X; 2];
        let cases: [(&str, Result<MorphSet, RenderError>); 6] = [
            ("position deltas", MorphSet::from_dense(3, &[(name("a"), &two, &[])], 0.0)),
            ("normal deltas", MorphSet::from_dense(2, &[(name("a"), &two, &[Vec3::Y])], 0.0)),
            ("finite", dense_one(&[Vec3::NAN, Vec3::X], &[], 0.0)),
            ("finite", dense_one(&two, &[Vec3::INFINITY, Vec3::Y], 0.0)),
            ("epsilon", dense_one(&two, &[], -1.0)),
            ("epsilon", dense_one(&two, &[], f32::NAN)),
        ];
        for (needle, result) in cases {
            let err = result.unwrap_err().to_string();
            assert!(err.contains(needle), "expected {needle:?} in {err:?}");
        }
    }

    #[test]
    fn a_nan_delta_is_an_error_rather_than_silently_dropped() {
        // `NaN > epsilon` is false, so a filter that only keeps "bigger" deltas would drop it unnoticed.
        assert!(dense_one(&[Vec3::NAN], &[], 0.5).is_err());
    }

    #[test]
    fn validate_checks_order_range_lengths_and_finiteness() {
        let ok = sparse(vec![0, 2], vec![Vec3::X; 2], vec![]);
        assert_eq!(MorphSet::from_targets(3, vec![ok.clone()]).map(|s| s.len()), Ok(1));
        let with_normals = SparseMorph { normals: vec![Vec3::Y; 2], ..ok.clone() };
        assert!(MorphSet::from_targets(3, vec![with_normals]).is_ok());

        let cases: [(&str, SparseMorph); 7] = [
            ("ascending", sparse(vec![2, 0], vec![Vec3::X; 2], vec![])),
            ("ascending", sparse(vec![1, 1], vec![Vec3::X; 2], vec![])),
            ("past the last", sparse(vec![0, 3], vec![Vec3::X; 2], vec![])),
            ("position deltas", sparse(vec![0, 1], vec![Vec3::X], vec![])),
            ("normal deltas", sparse(vec![0, 1], vec![Vec3::X; 2], vec![Vec3::Y])),
            ("finite", sparse(vec![0, 1], vec![Vec3::X, Vec3::NAN], vec![])),
            ("finite", sparse(vec![0, 1], vec![Vec3::X; 2], vec![Vec3::Y, Vec3::INFINITY])),
        ];
        for (needle, target) in cases {
            let err = MorphSet::from_targets(3, vec![target]).unwrap_err().to_string();
            assert!(err.contains(needle), "expected {needle:?} in {err:?}");
        }
    }

    #[test]
    fn validate_is_against_the_meshs_vertex_count() {
        let set = MorphSet::from_targets(8, vec![sparse(vec![7], vec![Vec3::X], vec![])]).unwrap();
        assert!(set.validate(8).is_ok());
        assert!(set.validate(7).is_err(), "the same set does not fit a smaller mesh");
    }

    #[test]
    fn the_default_options_are_a_plain_static_mesh() {
        assert_eq!(MeshOptions::default(), MeshOptions { usage: MeshUsage::Static, morphs: None });
    }
}
