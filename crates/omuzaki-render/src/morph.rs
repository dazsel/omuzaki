//! Morph targets: the CPU blend of an instance's weights into a per-instance vertex buffer.
//!
//! A mesh keeps its targets sparse ([`MorphSet`]). Each frame, an instance whose `morph` weights are not all zero gets
//! `position = base + Σ wᵢ·Δpositionᵢ` and `normal = normalize(base_normal + Σ wᵢ·Δnormalᵢ)` (the reference in
//! `omuzaki-render-mock`), written into a *shape stream* of its own that the passes bind in place of the mesh's. The
//! stream is then skinned by the vertex shader as usual, so morphing comes before skinning.
//!
//! **Pooling.** Streams are pooled by instance index: slot `i` serves instance `i` of the scene and keeps its buffer,
//! CPU copy and the weights it was last blended with. A frame whose weights, mesh and base vertices match the slot's
//! skips the blend and the upload; after warm-up nothing allocates. An instance with all-zero weights draws the mesh's
//! own stream, exactly as the reference does.
//!
//! **Known limits.** The outline pass pushes morphed vertices along the base mesh's smooth normals, and shadow fitting
//! uses the base mesh's bounds; both are right for the small deltas of blend shapes.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{MorphSet, RenderError, Scene};

use crate::arena::Arena;
use crate::resource::mesh::{GpuMesh, Shape};

/// The most targets one mesh may have (`Capabilities::max_morph_targets`).
pub(crate) const MAX_TARGETS: u32 = 256;

/// A mesh's morph data: its sparse targets, and the base vertices a static mesh no longer keeps elsewhere.
pub(crate) struct MeshMorphs {
    pub(crate) set: MorphSet,
    /// Empty for a dynamic mesh, whose current vertices are the base.
    pub(crate) base: Vec<Shape>,
}

/// `base` blended by `weights` into `out`, with `flags` (all false on entry and exit) marking the vertices whose
/// normal moved. Summed in target order in `f32` like the reference; a target of weight `0.0` is skipped.
fn blend(base: &[Shape], set: &MorphSet, weights: &[f32], flags: &mut Vec<bool>, out: &mut Vec<Shape>) {
    out.clear();
    out.extend_from_slice(base);
    if flags.len() < base.len() {
        flags.resize(base.len(), false);
    }
    let active = || set.targets().iter().zip(weights).filter(|(_, w)| **w != 0.0);
    for (target, &weight) in active() {
        for (k, &index) in target.indices.iter().enumerate() {
            let i = index as usize;
            let (Some(shape), Some(&delta)) = (out.get_mut(i), target.positions.get(k)) else { continue };
            shape.position = (Vec3::from(shape.position) + delta * weight).to_array();
            if let Some(&delta) = target.normals.get(k) {
                shape.normal = (Vec3::from(shape.normal) + delta * weight).to_array();
                if let Some(flag) = flags.get_mut(i) {
                    *flag = true;
                }
            }
        }
    }
    // Normalise once, after every target is summed; a normal that cancels out keeps its base value.
    for (target, _) in active().filter(|(t, _)| !t.normals.is_empty()) {
        for &index in &target.indices {
            let i = index as usize;
            if let Some(flag) = flags.get_mut(i).filter(|f| **f) {
                *flag = false;
                if let (Some(shape), Some(old)) = (out.get_mut(i), base.get(i)) {
                    shape.normal = Vec3::from(shape.normal).try_normalize().map_or(old.normal, |n| n.to_array());
                }
            }
        }
    }
}

/// What a slot's stream was blended from.
#[derive(Copy, Clone, PartialEq)]
struct Source {
    mesh: (u32, u32),
    /// The mesh's [`GpuMesh::vertex_version`] then.
    version: u64,
}

/// The stream of one instance slot.
#[derive(Default)]
struct Slot {
    buffer: Option<wgpu::Buffer>,
    shapes: Vec<Shape>,
    /// The weights `shapes` holds the blend of, valid while `source` is `Some`.
    weights: Vec<f32>,
    source: Option<Source>,
    /// The instance morphs this frame, so the passes bind `buffer`.
    active: bool,
}

impl Slot {
    /// Blends if `source` or `weights` differ from what the slot holds; true if it did.
    fn refresh(
        &mut self,
        source: Source,
        base: &[Shape],
        set: &MorphSet,
        weights: &[f32],
        flags: &mut Vec<bool>,
    ) -> bool {
        if self.source == Some(source) && self.weights == weights {
            return false;
        }
        blend(base, set, weights, flags, &mut self.shapes);
        self.weights.clear();
        self.weights.extend_from_slice(weights);
        self.source = Some(source);
        true
    }
}

/// The per-instance streams of the frame being drawn.
#[derive(Default)]
pub(crate) struct MorphPool {
    slots: Vec<Slot>,
    flags: Vec<bool>,
}

impl MorphPool {
    /// The stream instance `instance` draws with this frame, or `None` for the mesh's own.
    pub(crate) fn shapes(&self, instance: u32) -> Option<&wgpu::Buffer> {
        self.slots.get(instance as usize).filter(|s| s.active).and_then(|s| s.buffer.as_ref())
    }

    /// Blends and uploads the streams of `scene`'s morphed instances. Handles that miss are left for the frame to
    /// report. Nothing is written when this returns an error.
    ///
    /// # Errors
    /// `Invalid` for weights on a mesh without morph targets, or a count other than the mesh's target count.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        meshes: &Arena<GpuMesh>,
        scene: &Scene,
    ) -> Result<(), RenderError> {
        let weights_of = |i: usize| {
            let range = scene.instances.get(i)?.morph?;
            let start = range.start as usize;
            scene.morph_weights.get(start..start + range.len as usize)
        };
        for instance in &scene.instances {
            let Some(range) = instance.morph else { continue };
            let Some(mesh) = meshes.get(instance.mesh.index(), instance.mesh.generation()) else { continue };
            let Some((set, _)) = mesh.morph_input() else {
                return Err(RenderError::invalid("instance", "it has morph weights but its mesh has no morph targets"));
            };
            if range.len as usize != set.len() {
                let reason = format!("{} morph weights for a mesh with {} morph targets", range.len, set.len());
                return Err(RenderError::invalid("instance", reason));
            }
        }
        if self.slots.len() < scene.instances.len() {
            self.slots.resize_with(scene.instances.len(), Slot::default);
        }
        for (i, slot) in self.slots.iter_mut().enumerate() {
            slot.active = false;
            let (Some(instance), Some(weights)) = (scene.instances.get(i), weights_of(i)) else { continue };
            let Some(mesh) = meshes.get(instance.mesh.index(), instance.mesh.generation()) else { continue };
            let Some((set, base)) = mesh.morph_input() else { continue };
            if weights.iter().all(|w| *w == 0.0) {
                continue;
            }
            let source =
                Source { mesh: (instance.mesh.index(), instance.mesh.generation()), version: mesh.vertex_version() };
            let bytes = std::mem::size_of_val(base) as u64;
            if slot.buffer.as_ref().is_none_or(|b| b.size() != bytes) {
                slot.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("morphed shapes"),
                    size: bytes,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                slot.source = None;
            }
            if slot.refresh(source, base, set, weights, &mut self.flags)
                && let Some(buffer) = &slot.buffer
            {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&slot.shapes));
            }
            slot.active = true;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
mod tests {
    use omuzaki_core::id::Slug;
    use omuzaki_render_api::SparseMorph;
    use omuzaki_render_mock::morph_vertices;

    use super::*;

    fn set(n: usize) -> MorphSet {
        let slug = |s: &str| Slug::new(s).unwrap();
        let spread = |step: usize, scale: f32| -> SparseMorph {
            let indices: Vec<u32> = (0..n).step_by(step).map(|i| i as u32).collect();
            let positions = indices.iter().map(|&i| Vec3::new(scale, i as f32 * 0.01, -scale)).collect();
            let normals = indices.iter().map(|&i| Vec3::new(0.0, scale, i as f32 * 0.1)).collect();
            SparseMorph { name: slug("a"), indices, positions, normals }
        };
        let plain = SparseMorph { normals: vec![], ..spread(3, 0.5) };
        // A normal-only target at vertex 0, which the other two also move.
        let cancel =
            SparseMorph { name: slug("c"), indices: vec![0], positions: vec![Vec3::ZERO], normals: vec![-Vec3::Z] };
        MorphSet::from_targets(n, vec![spread(2, 0.25), plain, cancel]).unwrap()
    }

    fn base(n: usize) -> (Vec<Vec3>, Vec<Vec3>, Vec<Shape>) {
        let positions: Vec<Vec3> = (0..n).map(|i| Vec3::new(i as f32, 0.5, -1.0)).collect();
        let normals = vec![Vec3::Z; n];
        let shapes = positions
            .iter()
            .zip(&normals)
            .map(|(p, n)| Shape { position: p.to_array(), normal: n.to_array() })
            .collect();
        (positions, normals, shapes)
    }

    #[test]
    fn the_blend_equals_the_reference_within_1e_5() {
        let (positions, normals, shapes) = base(50);
        let set = set(50);
        let mut flags = Vec::new();
        let mut out = Vec::new();
        for weights in [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.5, -0.75, 0.0], [0.3, 0.9, 1.0], [2.0, 1.0, 1.0]] {
            blend(&shapes, &set, &weights, &mut flags, &mut out);
            let (want_p, want_n) = morph_vertices(&positions, &normals, &set, &weights).unwrap();
            for (i, shape) in out.iter().enumerate() {
                let (p, n) = (Vec3::from(shape.position), Vec3::from(shape.normal));
                assert!((p - want_p[i]).abs().max_element() < 1e-5, "{weights:?} position {i}");
                assert!((n - want_n[i]).abs().max_element() < 1e-5, "{weights:?} normal {i}");
            }
            assert!(flags.iter().all(|f| !f), "the scratch is left clean");
        }
    }

    #[test]
    fn zero_weights_leave_the_base_bit_for_bit() {
        let (_, _, shapes) = base(10);
        let (mut flags, mut out) = (Vec::new(), Vec::new());
        blend(&shapes, &set(10), &[0.0; 3], &mut flags, &mut out);
        assert_eq!(bytemuck::cast_slice::<_, u8>(&out), bytemuck::cast_slice::<_, u8>(&shapes));
    }

    #[test]
    fn an_unchanged_weight_set_skips_the_blend_and_a_change_does_not() {
        let (_, _, shapes) = base(10);
        let set = set(10);
        let (mut flags, mut slot) = (Vec::new(), Slot::default());
        let source = Source { mesh: (0, 0), version: 0 };
        assert!(slot.refresh(source, &shapes, &set, &[0.5, 0.0, 0.0], &mut flags), "first frame blends");
        assert!(!slot.refresh(source, &shapes, &set, &[0.5, 0.0, 0.0], &mut flags), "same weights skip");
        assert!(slot.refresh(source, &shapes, &set, &[0.6, 0.0, 0.0], &mut flags), "new weights blend");
        let moved = Source { version: 1, ..source };
        assert!(slot.refresh(moved, &shapes, &set, &[0.6, 0.0, 0.0], &mut flags), "moved base vertices blend");
        let other = Source { mesh: (1, 0), ..moved };
        assert!(slot.refresh(other, &shapes, &set, &[0.6, 0.0, 0.0], &mut flags), "another mesh blends");
    }
}
