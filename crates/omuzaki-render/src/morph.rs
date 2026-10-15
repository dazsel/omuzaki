//! Morph targets: the CPU blend of an instance's weights into a per-instance vertex buffer.
//!
//! A mesh keeps its targets sparse ([`MorphSet`]). Each frame, an instance whose `morph` weights are not all zero gets
//! `position = base + Σ wᵢ·Δpositionᵢ` and `normal = normalize(base_normal + Σ wᵢ·Δnormalᵢ)` (the reference in
//! `omuzaki-render-mock`), written into a *shape stream* of its own that the passes bind in place of the mesh's. The
//! stream is then skinned by the vertex shader as usual, so morphing comes before skinning.
//!
//! **Pooling.** Streams are pooled by instance index: slot `i` serves instance `i` of the scene and keeps its buffer,
//! CPU copy and the weights it was last blended with. A frame whose weights, mesh and base vertices match the slot's
//! skips the blend and the upload; after warm-up nothing allocates. A buffer only grows, so a slot that alternates
//! between meshes of different sizes keeps the larger one. An instance with all-zero weights draws the mesh's own
//! stream, exactly as the reference does.
//!
//! **What follows the blend**, as it does for [`update_vertices`](crate::Renderer::update_vertices): the slot's bounds
//! (shadow fitting and the transparent sort read them in place of the mesh's) are refitted whenever it re-blends, and
//! for an instance whose material outlines, its smooth normals are re-welded from the blended vertices with the mesh's
//! weld table, so the hull follows the morph without a second weld. As for a dynamic mesh, those welds are the ones
//! found at creation: a morph that pulls a seam apart or brings two points together does not change which vertices
//! share an outline direction.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Instance, MorphSet, RenderError, Scene};

use crate::arena::Arena;
use crate::resource::mesh::{GpuMesh, Shape, Surface, resmooth};
use crate::shadow::bounds::MeshBounds;

/// The most targets one mesh may have (`Capabilities::max_morph_targets`).
pub(crate) const MAX_TARGETS: u32 = 256;

/// A mesh's morph data: its sparse targets, and what a static mesh no longer keeps elsewhere (a dynamic mesh's
/// vertices, skin data and welds are its shadow copy's).
pub(crate) struct MeshMorphs {
    pub(crate) set: MorphSet,
    pub(crate) base: Vec<Shape>,
    pub(crate) skin: Vec<Surface>,
    pub(crate) welds: Vec<usize>,
    pub(crate) weld_count: usize,
}

/// What blending a mesh's targets reads.
pub(crate) struct MorphInput<'a> {
    pub(crate) set: &'a MorphSet,
    /// The vertices the targets apply to: the mesh's current ones.
    pub(crate) base: &'a [Shape],
    /// The skin data, for the blend's joint bounds; empty for a rigid mesh.
    pub(crate) skin: &'a [Surface],
    /// The weld slot of every vertex, for the outline directions.
    pub(crate) welds: &'a [usize],
    pub(crate) weld_count: usize,
    pub(crate) joints_needed: u32,
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
            // The set was validated against this vertex count when the mesh was created, so a miss is a broken invariant.
            let (Some(shape), Some(&delta)) = (out.get_mut(i), target.positions.get(k)) else {
                debug_assert!(false, "morph target {:?}: entry {k} (vertex {i}) does not fit the mesh", target.name);
                continue;
            };
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

/// A buffer that only grows: its `size()` is the capacity, and `used` bytes of it hold the current contents.
#[derive(Default)]
struct Stream {
    buffer: Option<wgpu::Buffer>,
    used: u64,
}

impl Stream {
    /// Makes room for `bytes`, replacing the buffer only if it is smaller. True if it was replaced (its contents are
    /// gone).
    fn reserve(&mut self, device: &wgpu::Device, label: &str, bytes: u64) -> bool {
        self.used = bytes;
        if self.buffer.as_ref().is_some_and(|b| b.size() >= bytes) {
            return false;
        }
        self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        true
    }

    fn slice(&self) -> Option<wgpu::BufferSlice<'_>> {
        self.buffer.as_ref().map(|b| b.slice(..self.used))
    }
}

/// Frames a slot may go unused before its buffers are released: long enough that a scene which toggles an instance's
/// morph, or alternates between a few scenes, keeps its buffers, short enough that a scene that stopped morphing
/// (or got smaller) does not hold its memory for good.
const IDLE_FRAMES: u32 = 120;

/// The stream of one instance slot.
struct Slot {
    shapes: Stream,
    /// The CPU copy of `shapes`.
    blended: Vec<Shape>,
    /// The outline directions welded from `blended`, valid unless `smooth_stale`.
    smooth: Stream,
    smooth_cpu: Vec<[f32; 3]>,
    /// The weights `blended` holds the blend of, valid while `source` is `Some`.
    weights: Vec<f32>,
    source: Option<Source>,
    /// The boxes of `blended`, for shadow fitting and the transparent sort.
    bounds: MeshBounds,
    /// `smooth` lags `blended`; it is brought up to date when an outlined instance needs it.
    smooth_stale: bool,
    /// The instance morphs this frame, so the passes bind the slot's streams.
    active: bool,
    /// Frames since the slot was last active, for [`IDLE_FRAMES`].
    idle: u32,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            shapes: Stream::default(),
            blended: Vec::new(),
            smooth: Stream::default(),
            smooth_cpu: Vec::new(),
            weights: Vec::new(),
            source: None,
            bounds: MeshBounds::empty(),
            smooth_stale: true,
            active: false,
            idle: 0,
        }
    }
}

impl Slot {
    /// Blends if `source` or `weights` differ from what the slot holds, refitting the bounds; true if it did.
    fn refresh(&mut self, source: Source, input: &MorphInput<'_>, weights: &[f32], flags: &mut Vec<bool>) -> bool {
        if self.source == Some(source) && self.weights == weights {
            return false;
        }
        blend(input.base, input.set, weights, flags, &mut self.blended);
        self.bounds.refit(input.joints_needed, &self.blended, input.skin);
        self.weights.clear();
        self.weights.extend_from_slice(weights);
        self.source = Some(source);
        self.smooth_stale = true;
        true
    }
}

/// The per-instance streams of the frame being drawn.
#[derive(Default)]
pub(crate) struct MorphPool {
    slots: Vec<Slot>,
    flags: Vec<bool>,
    sums: Vec<Vec3>,
}

impl MorphPool {
    fn active(&self, instance: u32) -> Option<&Slot> {
        self.slots.get(instance as usize).filter(|s| s.active)
    }

    /// The GPU memory the pool holds: every slot's buffers at their capacity.
    pub(crate) fn bytes(&self) -> u64 {
        let size = |stream: &Stream| stream.buffer.as_ref().map_or(0, wgpu::Buffer::size);
        self.slots.iter().map(|s| size(&s.shapes) + size(&s.smooth)).sum()
    }

    /// Drops every slot last blended from the mesh at `index` and `generation`, buffers and CPU copies with them, so
    /// releasing a mesh frees what was sized for it. A slot last blended from a live mesh keeps its capacity.
    pub(crate) fn forget(&mut self, index: u32, generation: u32) {
        for slot in &mut self.slots {
            if slot.source.is_some_and(|s| s.mesh == (index, generation)) {
                *slot = Slot::default();
            }
        }
        self.release_scratch_if_unused();
    }

    /// Gives back the scratch (sized for the largest mesh blended) once no slot holds a blend.
    fn release_scratch_if_unused(&mut self) {
        if self.slots.iter().all(|s| s.source.is_none()) {
            self.flags = Vec::new();
            self.sums = Vec::new();
        }
    }

    /// The shape stream instance `instance` draws with this frame, or `None` for the mesh's own.
    pub(crate) fn shapes(&self, instance: u32) -> Option<wgpu::BufferSlice<'_>> {
        self.active(instance)?.shapes.slice()
    }

    /// The shape and outline-direction streams of a morphing instance, if it has both (the outline pass's).
    pub(crate) fn outline_streams(&self, instance: u32) -> Option<(wgpu::BufferSlice<'_>, wgpu::BufferSlice<'_>)> {
        let slot = self.active(instance).filter(|s| !s.smooth_stale)?;
        Some((slot.shapes.slice()?, slot.smooth.slice()?))
    }

    /// The bounds of the instance's blended vertices, or `None` for the mesh's own.
    pub(crate) fn bounds(&self, instance: u32) -> Option<&MeshBounds> {
        self.active(instance).map(|s| &s.bounds)
    }

    /// Blends and uploads the streams of `scene`'s morphed instances, and the outline directions of those for which
    /// `outlined` is true. Handles that miss are left for the frame to report. Nothing is written when this returns an
    /// error.
    ///
    /// # Errors
    /// `Invalid` for weights on a mesh without morph targets, or a count other than the mesh's target count.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        meshes: &Arena<GpuMesh>,
        scene: &Scene,
        outlined: impl Fn(&Instance) -> bool,
    ) -> Result<(), RenderError> {
        let weights_of = |instance: &Instance| {
            let range = instance.morph?;
            let start = range.start as usize;
            scene.morph_weights.get(start..start + range.len as usize)
        };
        for instance in &scene.instances {
            let Some(range) = instance.morph else { continue };
            let Some(mesh) = meshes.get(instance.mesh.index(), instance.mesh.generation()) else { continue };
            let Some(input) = mesh.morph_input() else {
                return Err(RenderError::invalid("instance", "it has morph weights but its mesh has no morph targets"));
            };
            if range.len as usize != input.set.len() {
                let reason = format!("{} morph weights for a mesh with {} morph targets", range.len, input.set.len());
                return Err(RenderError::invalid("instance", reason));
            }
        }
        if self.slots.len() < scene.instances.len() {
            self.slots.resize_with(scene.instances.len(), Slot::default);
        }
        for (slot, instance) in
            self.slots.iter_mut().zip(scene.instances.iter().map(Some).chain(std::iter::repeat(None)))
        {
            slot.active = false;
            let morphing = instance.and_then(|instance| {
                let weights = weights_of(instance).filter(|w| w.iter().any(|w| *w != 0.0))?;
                let mesh = meshes.get(instance.mesh.index(), instance.mesh.generation())?;
                Some((instance, weights, mesh, mesh.morph_input()?))
            });
            let Some((instance, weights, mesh, input)) = morphing else {
                slot.idle = slot.idle.saturating_add(1);
                if slot.idle > IDLE_FRAMES {
                    *slot = Slot::default();
                }
                continue;
            };
            slot.idle = 0;
            let source =
                Source { mesh: (instance.mesh.index(), instance.mesh.generation()), version: mesh.vertex_version() };
            if slot.shapes.reserve(device, "morphed shapes", std::mem::size_of_val(input.base) as u64) {
                slot.source = None;
            }
            if slot.refresh(source, &input, weights, &mut self.flags)
                && let Some(buffer) = &slot.shapes.buffer
            {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&slot.blended));
            }
            if slot.smooth_stale && outlined(instance) {
                let bytes = (input.base.len() * size_of::<[f32; 3]>()) as u64;
                slot.smooth.reserve(device, "morphed outline directions", bytes);
                slot.smooth_cpu.clear();
                slot.smooth_cpu.resize(input.base.len(), [0.0; 3]);
                resmooth(input.welds, input.weld_count, &mut self.sums, &slot.blended, &mut slot.smooth_cpu);
                if let Some(buffer) = &slot.smooth.buffer {
                    queue.write_buffer(buffer, 0, bytemuck::cast_slice(&slot.smooth_cpu));
                }
                slot.smooth_stale = false;
            }
            slot.active = true;
        }
        self.release_scratch_if_unused();
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
        let input = MorphInput { set: &set, base: &shapes, skin: &[], welds: &[], weld_count: 0, joints_needed: 0 };
        let (mut flags, mut slot) = (Vec::new(), Slot::default());
        let source = Source { mesh: (0, 0), version: 0 };
        assert!(slot.refresh(source, &input, &[0.5, 0.0, 0.0], &mut flags), "first frame blends");
        assert!(!slot.refresh(source, &input, &[0.5, 0.0, 0.0], &mut flags), "same weights skip");
        assert!(slot.refresh(source, &input, &[0.6, 0.0, 0.0], &mut flags), "new weights blend");
        let moved = Source { version: 1, ..source };
        assert!(slot.refresh(moved, &input, &[0.6, 0.0, 0.0], &mut flags), "moved base vertices blend");
        let other = Source { mesh: (1, 0), ..moved };
        assert!(slot.refresh(other, &input, &[0.6, 0.0, 0.0], &mut flags), "another mesh blends");
    }

    #[test]
    fn a_re_blend_refits_the_slots_bounds_and_marks_the_directions_stale() {
        let (_, _, shapes) = base(10);
        let set = set(10);
        let input = MorphInput { set: &set, base: &shapes, skin: &[], welds: &[], weld_count: 0, joints_needed: 0 };
        let (mut flags, mut slot) = (Vec::new(), Slot::default());
        slot.smooth_stale = false;
        let source = Source { mesh: (0, 0), version: 0 };
        let bounds = |slot: &Slot| slot.bounds.instance(&omuzaki_core::glam::Mat4::IDENTITY, None);
        slot.refresh(source, &input, &[0.0, 0.0, 0.0], &mut flags);
        let rest = bounds(&slot);
        assert!(slot.smooth_stale, "a blend leaves the outline directions behind");
        slot.refresh(source, &input, &[4.0, 0.0, 0.0], &mut flags);
        assert!(bounds(&slot).max.y > rest.max.y, "the box follows the blend: {rest:?} then {:?}", bounds(&slot));
    }

    #[test]
    fn a_stream_grows_but_never_shrinks() {
        let gpu = crate::gpu::Gpu::open_headless(crate::gpu::HeadlessOptions {
            adapter: crate::gpu::AdapterPreference::Software,
        })
        .unwrap();
        let mut stream = Stream::default();
        assert!(stream.reserve(&gpu.device, "test", 240), "the first reserve creates the buffer");
        let big = stream.buffer.clone().unwrap();
        assert!(!stream.reserve(&gpu.device, "test", 120), "a smaller mesh reuses it");
        assert_eq!(stream.used, 120);
        assert!(!stream.reserve(&gpu.device, "test", 240), "and the larger one comes back to it");
        assert_eq!(stream.buffer.as_ref(), Some(&big));
        assert!(stream.reserve(&gpu.device, "test", 480), "only a bigger mesh replaces it");
    }

    #[test]
    fn two_prewarmed_scenes_alternating_meshes_of_different_sizes_create_no_buffers() {
        use omuzaki_core::glam::Mat4;
        use omuzaki_render_api::{Camera, MaterialId, MeshId, MeshOptions, MorphRange};

        let gpu = crate::gpu::Gpu::open_headless(crate::gpu::HeadlessOptions {
            adapter: crate::gpu::AdapterPreference::Software,
        })
        .unwrap();
        let mut meshes = Arena::default();
        let mut scenes = Vec::new();
        // A cube (24 vertices) and a quad (4): one slot, two sizes.
        for mesh in [
            omuzaki_render_mock::shapes::cuboid(Vec3::ONE),
            omuzaki_render_mock::shapes::quad(omuzaki_core::glam::Vec2::ONE),
        ] {
            let n = mesh.positions.len();
            let deltas = vec![Vec3::X; n];
            let set = MorphSet::from_dense(n, &[(Slug::new("a").unwrap(), &deltas, &[])], 0.0).unwrap();
            let options = MeshOptions { morphs: Some(set), ..MeshOptions::default() };
            let (index, generation) = meshes.insert(GpuMesh::upload(&gpu, &mesh, &options).unwrap());
            let mut scene = Scene::new(Camera::default());
            let id = MeshId::from_parts(index, generation);
            let mut instance = Instance::new(id, MaterialId::from_parts(0, 0), Mat4::IDENTITY);
            instance.morph = Some(MorphRange { start: 0, len: 1 });
            scene.instances.push(instance);
            scene.morph_weights.push(0.5);
            scenes.push(scene);
        }
        let mut pool = MorphPool::default();
        let mut buffers = Vec::new();
        for frame in 0..8 {
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &scenes[frame % 2], |_| false).unwrap();
            buffers.push(pool.slots[0].shapes.buffer.clone().unwrap());
            assert!(pool.shapes(0).is_some());
        }
        // Frame 0 sized the buffer for the cube; every later frame, on either mesh, reuses it.
        assert!(buffers.iter().all(|b| b == &buffers[0]), "a slot alternating between meshes reallocated");
    }

    /// A pool with one morphed cube in slot 0 of `scene`, blended once, and an empty scene.
    fn warmed_pool(gpu: &crate::gpu::Gpu) -> (MorphPool, Arena<GpuMesh>, (u32, u32), Scene) {
        use omuzaki_core::glam::Mat4;
        use omuzaki_render_api::{Camera, MaterialId, MeshId, MeshOptions, MorphRange};

        let cube = omuzaki_render_mock::shapes::cuboid(Vec3::ONE);
        let n = cube.positions.len();
        let deltas = vec![Vec3::X; n];
        let set = MorphSet::from_dense(n, &[(Slug::new("a").unwrap(), &deltas, &[])], 0.0).unwrap();
        let mut meshes = Arena::default();
        let (index, generation) = meshes
            .insert(GpuMesh::upload(gpu, &cube, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }).unwrap());
        let mut scene = Scene::new(Camera::default());
        let mut instance =
            Instance::new(MeshId::from_parts(index, generation), MaterialId::from_parts(0, 0), Mat4::IDENTITY);
        instance.morph = Some(MorphRange { start: 0, len: 1 });
        scene.instances.push(instance);
        scene.morph_weights.push(0.5);
        let mut pool = MorphPool::default();
        pool.prepare(&gpu.device, &gpu.queue, &meshes, &scene, |_| true).unwrap();
        (pool, meshes, (index, generation), scene)
    }

    #[test]
    fn forgetting_a_mesh_frees_its_slots_and_the_scratch() {
        let gpu = crate::gpu::Gpu::open_headless(crate::gpu::HeadlessOptions {
            adapter: crate::gpu::AdapterPreference::Software,
        })
        .unwrap();
        let (mut pool, _meshes, (index, generation), _scene) = warmed_pool(&gpu);
        assert!(pool.bytes() > 0 && !pool.flags.is_empty(), "the blend holds buffers and scratch");
        pool.forget(index, generation);
        assert_eq!((pool.bytes(), pool.flags.capacity(), pool.sums.capacity()), (0, 0, 0));
    }

    #[test]
    fn a_slot_unused_for_a_while_gives_its_buffers_back() {
        let gpu = crate::gpu::Gpu::open_headless(crate::gpu::HeadlessOptions {
            adapter: crate::gpu::AdapterPreference::Software,
        })
        .unwrap();
        let (mut pool, meshes, _, scene) = warmed_pool(&gpu);
        let empty = Scene::new(omuzaki_render_api::Camera::default());
        for _ in 0..IDLE_FRAMES {
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &empty, |_| true).unwrap();
        }
        assert!(pool.bytes() > 0, "a short pause keeps the buffers");
        pool.prepare(&gpu.device, &gpu.queue, &meshes, &scene, |_| true).unwrap();
        assert_eq!(pool.slots[0].idle, 0, "use resets the count");
        for _ in 0..=IDLE_FRAMES {
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &empty, |_| true).unwrap();
        }
        assert_eq!((pool.bytes(), pool.flags.capacity()), (0, 0), "a long one frees them, scratch included");
    }
}
