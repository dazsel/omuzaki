//! Morph targets: blending an instance's weights into a per-instance vertex buffer, on the GPU or the CPU.
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
//!
//! **Two paths, one pool.** Where the adapter has compute the blend is a compute pass ([`gpu`], `shader/morph.wgsl`): a
//! mesh's sparse targets are uploaded once, when it is created, and a frame uploads only the weights; the pass writes
//! into the same pooled buffers, so the change-skip, the stats and the release are the same mechanism. Otherwise
//! ([`cpu`]) the CPU blends and uploads the result; it is also the reference the GPU path is tested against. The path is
//! chosen when the renderer is created ([`MorphPool::new`]) and never changes: there is no fallback mid-session. The
//! GPU path reads nothing back, so an instance's bounds are the mesh's grown by what the weights can move a vertex
//! (conservative, never smaller than the CPU path's exact ones), and its outline directions are re-welded by a second
//! dispatch, only when the instance re-blends.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Instance, MorphSet, RenderError, Scene};

mod cpu;
mod gpu;

use crate::arena::Arena;
use crate::resource::mesh::{GpuMesh, Shape, Surface, resmooth};
use crate::shadow::bounds::MeshBounds;
use cpu::blend;

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

/// Which path blends: chosen once, when the renderer is created.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum MorphPath {
    /// A compute pass.
    Gpu,
    /// The CPU, then an upload.
    Cpu,
}

impl MorphPath {
    /// The name the diagnostics use.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
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
    fn reserve(&mut self, device: &wgpu::Device, label: &str, bytes: u64, usage: wgpu::BufferUsages) -> bool {
        self.used = bytes;
        if self.buffer.as_ref().is_some_and(|b| b.size() >= bytes) {
            return false;
        }
        self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes,
            usage,
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
    /// The GPU path's side: the params buffer and bind groups.
    gpu: Option<gpu::SlotGpu>,
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
            gpu: None,
        }
    }
}

impl Slot {
    /// Whether the slot holds a blend or any buffer, i.e. anything worth releasing.
    fn holds_memory(&self) -> bool {
        self.source.is_some() || self.shapes.buffer.is_some() || self.smooth.buffer.is_some() || self.gpu.is_some()
    }

    /// Whether the slot already holds the blend of `weights` from `source`.
    fn holds(&self, source: Source, weights: &[f32]) -> bool {
        self.source == Some(source) && self.weights == weights
    }

    /// Notes that the slot now holds the blend of `weights` from `source`; the outline directions lag it.
    fn blended_from(&mut self, source: Source, weights: &[f32]) {
        self.weights.clear();
        self.weights.extend_from_slice(weights);
        self.source = Some(source);
        self.smooth_stale = true;
    }

    /// The CPU path: blends if `source` or `weights` differ from what the slot holds, refitting the bounds; true if it
    /// did.
    fn refresh(&mut self, source: Source, input: &MorphInput<'_>, weights: &[f32], flags: &mut Vec<bool>) -> bool {
        if self.holds(source, weights) {
            return false;
        }
        blend(input.base, input.set, weights, flags, &mut self.blended);
        self.bounds.refit(input.joints_needed, &self.blended, input.skin);
        self.blended_from(source, weights);
        true
    }
}

/// An instance that morphs this frame: weights, not all zero, on a live mesh that has targets.
struct Morphing<'a> {
    instance: &'a Instance,
    weights: &'a [f32],
    mesh: &'a GpuMesh,
    input: MorphInput<'a>,
}

/// `instance`'s morph, if it has one this frame. The one rule for what needs a slot, shared by the walk that fills
/// the slots and the count of how many there must be.
fn morphing<'a>(instance: &'a Instance, scene: &'a Scene, meshes: &'a Arena<GpuMesh>) -> Option<Morphing<'a>> {
    let range = instance.morph?;
    let start = range.start as usize;
    let weights = scene.morph_weights.get(start..start + range.len as usize)?;
    if weights.iter().all(|w| *w == 0.0) {
        return None;
    }
    let mesh = meshes.get(instance.mesh.index(), instance.mesh.generation())?;
    Some(Morphing { instance, weights, mesh, input: mesh.morph_input()? })
}

/// The per-instance streams of the frame being drawn.
#[derive(Default)]
pub(crate) struct MorphPool {
    slots: Vec<Slot>,
    flags: Vec<bool>,
    sums: Vec<Vec3>,
    /// The GPU path's pipelines, or `None` for the CPU path (the default).
    gpu: Option<gpu::Pipelines>,
    /// The GPU path's data per mesh slot: the mesh's generation and what was uploaded for it.
    meshes_gpu: Vec<Option<(u32, gpu::MeshGpu)>>,
    /// The dispatches to record this frame, in order.
    pending: Vec<gpu::Pending>,
}

impl MorphPool {
    /// A pool on the GPU path if `device` can run it, else on the CPU path.
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        Self { gpu: gpu::supported(device).then(|| gpu::Pipelines::new(device)), ..Self::default() }
    }

    /// The path that blends.
    pub(crate) fn path(&self) -> MorphPath {
        if self.gpu.is_some() { MorphPath::Gpu } else { MorphPath::Cpu }
    }

    /// Switches to the CPU path on an adapter that has compute: a test hook.
    ///
    /// # Errors
    /// `Invalid` once a mesh with morph targets exists, since its data is on the GPU: the path never changes under a
    /// live mesh.
    pub(crate) fn force_cpu(&mut self) -> Result<(), RenderError> {
        if self.meshes_gpu.iter().any(Option::is_some) {
            return Err(RenderError::invalid("morph path", "meshes with morph targets exist; choose the path first"));
        }
        self.gpu = None;
        Ok(())
    }

    /// Uploads the GPU path's data for the mesh just created at `index` and `generation`; nothing on the CPU path.
    ///
    /// # Errors
    /// `LimitExceeded` for a mesh the GPU path cannot hold.
    pub(crate) fn attach(
        &mut self,
        device: &wgpu::Device,
        index: u32,
        generation: u32,
        mesh: &GpuMesh,
    ) -> Result<(), RenderError> {
        let (Some(pipelines), Some(input)) = (&self.gpu, mesh.morph_input()) else { return Ok(()) };
        let data = gpu::MeshGpu::new(device, pipelines, mesh.shapes_buffer(), &input)?;
        let at = index as usize;
        if self.meshes_gpu.len() <= at {
            self.meshes_gpu.resize_with(at + 1, || None);
        }
        if let Some(entry) = self.meshes_gpu.get_mut(at) {
            *entry = Some((generation, data));
        }
        Ok(())
    }

    /// Records this frame's dispatches into `encoder`, before the passes that draw the streams.
    pub(crate) fn record(&self, encoder: &mut wgpu::CommandEncoder) {
        let Some(pipelines) = &self.gpu else { return };
        if self.pending.is_empty() {
            return;
        }
        gpu::record(
            encoder,
            pipelines,
            &self.pending,
            |slot| self.slots.get(slot)?.gpu.as_ref(),
            |mesh| self.meshes_gpu.get(mesh)?.as_ref().map(|(_, data)| data),
        );
    }

    fn active(&self, instance: u32) -> Option<&Slot> {
        self.slots.get(instance as usize).filter(|s| s.active)
    }

    /// The GPU memory the pool holds: every slot's buffers at their capacity, and the GPU path's data per mesh.
    pub(crate) fn bytes(&self) -> u64 {
        let size = |stream: &Stream| stream.buffer.as_ref().map_or(0, wgpu::Buffer::size);
        let slots: u64 = self
            .slots
            .iter()
            .map(|s| size(&s.shapes) + size(&s.smooth) + s.gpu.as_ref().map_or(0, |g| g.params.size()))
            .sum();
        slots + self.meshes_gpu.iter().flatten().map(|(_, data)| data.bytes).sum::<u64>()
    }

    /// Drops every slot last blended from the mesh at `index` and `generation`, buffers and CPU copies with them, and
    /// the GPU path's data for it, so releasing a mesh frees what was sized for it. A slot last blended from a live
    /// mesh keeps its capacity.
    pub(crate) fn forget(&mut self, index: u32, generation: u32) {
        if let Some(entry) =
            self.meshes_gpu.get_mut(index as usize).filter(|e| e.as_ref().is_some_and(|(g, _)| *g == generation))
        {
            *entry = None;
        }
        while self.meshes_gpu.last().is_some_and(Option::is_none) {
            self.meshes_gpu.pop();
        }
        if self.meshes_gpu.capacity() > 2 * self.meshes_gpu.len() {
            self.meshes_gpu.shrink_to_fit();
        }
        for slot in &mut self.slots {
            if slot.source.is_some_and(|s| s.mesh == (index, generation)) {
                *slot = Slot::default();
            }
        }
        self.trim();
    }

    /// Shrinks what grows with the scene or the mesh back to what the slots still hold: trailing empty slots go, and
    /// the scratch (sized for the largest mesh blended) shrinks to the largest blend left, to nothing if there is none.
    fn trim(&mut self) {
        // Only slots that hold nothing (evicted, or never used) go; one that is merely inactive this frame keeps its
        // place until it idles out. The vector gives its room back only once it is more than twice what it needs, so
        // a scene that shrinks and grows again does not reallocate it.
        while self.slots.last().is_some_and(|s| !s.active && !s.holds_memory()) {
            self.slots.pop();
        }
        if self.slots.capacity() > 2 * self.slots.len() {
            self.slots.shrink_to_fit();
        }
        // The dispatch list holds at most a blend and a weld for each slot.
        self.pending.shrink_to(2 * self.slots.len());
        // Capacity follows what the slots keep, not this frame's use: a slot that alternates between a large and a small
        // mesh keeps the large one's room, so the scratch must too, or it would be reallocated every large frame.
        let largest = self.slots.iter().map(|s| s.blended.capacity()).max().unwrap_or(0);
        if self.flags.capacity() > largest {
            self.flags.truncate(largest);
            self.flags.shrink_to(largest);
        }
        if self.sums.capacity() > largest {
            // Scratch: cleared before each use, so there is nothing to keep.
            self.sums.clear();
            self.sums.shrink_to(largest);
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
        // Slots reach only as far as the last instance that morphs this frame, so one whose weights are all zero needs
        // none, and a scene that shrinks gives its tail back.
        let needed = scene.instances.iter().rposition(|i| morphing(i, scene, meshes).is_some()).map_or(0, |i| i + 1);
        if self.slots.len() < needed {
            self.slots.resize_with(needed, Slot::default);
        }
        let Self { slots, flags, sums, gpu, meshes_gpu, pending, .. } = self;
        pending.clear();
        let usage = wgpu::BufferUsages::VERTEX
            | wgpu::BufferUsages::COPY_DST
            | if gpu.is_some() { wgpu::BufferUsages::STORAGE } else { wgpu::BufferUsages::empty() };
        // The unit tests read the streams back to compare the two paths; nothing else ever does.
        #[cfg(test)]
        let usage = usage | wgpu::BufferUsages::COPY_SRC;
        for (index, (slot, instance)) in
            slots.iter_mut().zip(scene.instances.iter().map(Some).chain(std::iter::repeat(None))).enumerate()
        {
            slot.active = false;
            let Some(Morphing { instance, weights, mesh, input }) =
                instance.and_then(|instance| morphing(instance, scene, meshes))
            else {
                // Counts only while there is something to release, so a released slot is reset once, not every frame.
                if slot.holds_memory() {
                    slot.idle += 1;
                    if slot.idle > IDLE_FRAMES {
                        *slot = Slot::default();
                    }
                }
                continue;
            };
            slot.idle = 0;
            let mesh_slot = instance.mesh.index();
            let source = Source { mesh: (mesh_slot, instance.mesh.generation()), version: mesh.vertex_version() };
            if slot.shapes.reserve(device, "morphed shapes", std::mem::size_of_val(input.base) as u64, usage) {
                slot.source = None;
                if let Some(on_gpu) = &mut slot.gpu {
                    on_gpu.forget_groups();
                }
            }
            let wanted_outline = outlined(instance);
            match gpu {
                None => {
                    blend_on_cpu(slot, &input, weights, source, wanted_outline, (device, queue), usage, (flags, sums));
                }
                Some(pipelines) => {
                    let Some(data) = meshes_gpu
                        .get(mesh_slot as usize)
                        .and_then(Option::as_ref)
                        .filter(|(generation, _)| *generation == instance.mesh.generation())
                        .map(|(_, data)| data)
                    else {
                        // `attach` ran when the mesh was created, so a mesh with targets always has its data.
                        debug_assert!(false, "a morphed mesh without GPU data");
                        continue;
                    };
                    let target = gpu::Target { pipelines, data, mesh: mesh_slot as usize, slot: index };
                    blend_on_gpu(slot, mesh, weights, source, wanted_outline, &target, (device, queue), usage, pending);
                }
            }
            slot.active = true;
        }
        self.trim();
        Ok(())
    }
}

/// The CPU path for one morphing instance: blends and uploads if the slot's blend is stale, then re-welds the outline
/// directions if `outlined` and they lag.
#[allow(clippy::too_many_arguments)]
fn blend_on_cpu(
    slot: &mut Slot,
    input: &MorphInput<'_>,
    weights: &[f32],
    source: Source,
    outlined: bool,
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    usage: wgpu::BufferUsages,
    (flags, sums): (&mut Vec<bool>, &mut Vec<Vec3>),
) {
    if slot.refresh(source, input, weights, flags)
        && let Some(buffer) = &slot.shapes.buffer
    {
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(&slot.blended));
    }
    if slot.smooth_stale && outlined {
        let vertices = input.base.len();
        slot.smooth.reserve(device, "morphed outline directions", (vertices * size_of::<[f32; 3]>()) as u64, usage);
        slot.smooth_cpu.clear();
        slot.smooth_cpu.resize(vertices, [0.0; 3]);
        resmooth(input.welds, input.weld_count, sums, &slot.blended, &mut slot.smooth_cpu);
        if let Some(buffer) = &slot.smooth.buffer {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&slot.smooth_cpu));
        }
        slot.smooth_stale = false;
    }
}

/// The GPU path for one morphing instance: uploads the weights and queues the blend if the slot's is stale, grows its
/// bounds, and queues the outline re-weld if `outlined` and the directions lag.
#[allow(clippy::too_many_arguments)]
fn blend_on_gpu(
    slot: &mut Slot,
    mesh: &GpuMesh,
    weights: &[f32],
    source: Source,
    outlined: bool,
    target: &gpu::Target<'_>,
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    usage: wgpu::BufferUsages,
    pending: &mut Vec<gpu::Pending>,
) {
    let mut on_gpu = slot.gpu.take().unwrap_or_else(|| gpu::SlotGpu::new(device));
    if let Some(shapes) = &slot.shapes.buffer {
        on_gpu.bind_blend(device, target.pipelines, shapes);
    }
    if !slot.holds(source, weights) {
        on_gpu.write_params(queue, target.data.vertices, weights);
        let (down, up) = gpu::grow(weights, target.data);
        slot.bounds.set_grown(&mesh.bounds, down, up);
        slot.blended_from(source, weights);
        pending.push(target.pending(gpu::Step::Blend));
    }
    if slot.smooth_stale && outlined {
        let bytes = (target.data.vertices as usize * size_of::<[f32; 3]>()) as u64;
        if slot.smooth.reserve(device, "morphed outline directions", bytes, usage) {
            on_gpu.forget_weld();
        }
        if let (Some(shapes), Some(smooth)) = (&slot.shapes.buffer, &slot.smooth.buffer) {
            on_gpu.bind_weld(device, target.pipelines, shapes, smooth);
        }
        pending.push(target.pending(gpu::Step::Weld));
        slot.smooth_stale = false;
    }
    slot.gpu = Some(on_gpu);
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
        assert!(
            stream.reserve(&gpu.device, "test", 240, wgpu::BufferUsages::VERTEX),
            "the first reserve creates the buffer"
        );
        let big = stream.buffer.clone().unwrap();
        assert!(!stream.reserve(&gpu.device, "test", 120, wgpu::BufferUsages::VERTEX), "a smaller mesh reuses it");
        assert_eq!(stream.used, 120);
        assert!(
            !stream.reserve(&gpu.device, "test", 240, wgpu::BufferUsages::VERTEX),
            "and the larger one comes back to it"
        );
        assert_eq!(stream.buffer.as_ref(), Some(&big));
        assert!(stream.reserve(&gpu.device, "test", 480, wgpu::BufferUsages::VERTEX), "only a bigger mesh replaces it");
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
        let (mut scenes, mut ids) = (Vec::new(), Vec::new());
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
            ids.push((index, generation));
        }
        let mut pool = MorphPool::default();
        let (mut buffers, mut scratch) = (Vec::new(), Vec::new());
        for frame in 0..8 {
            // Outlined, so the per-point sums are used too.
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &scenes[frame % 2], |_| true).unwrap();
            buffers.push((pool.slots[0].shapes.buffer.clone().unwrap(), pool.slots[0].smooth.buffer.clone().unwrap()));
            scratch.push((pool.flags.capacity(), pool.sums.capacity()));
            assert!(pool.shapes(0).is_some());
        }
        // Frame 0 sized the buffers for the cube; every later frame, on either mesh, reuses them.
        assert!(buffers.iter().all(|b| b == &buffers[0]), "a slot alternating between meshes reallocated");
        // Capacity follows what the slot keeps, not the frame's mesh: the scratch holds steady across the alternation.
        assert!(scratch.iter().all(|c| *c == scratch[0]) && scratch[0].0 >= 24, "scratch capacities: {scratch:?}");
        for (index, generation) in ids {
            pool.forget(index, generation);
        }
        assert_eq!((pool.flags.capacity(), pool.sums.capacity(), pool.bytes()), (0, 0, 0), "forgetting frees it all");
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
        assert_eq!(pool.slots.len(), 0, "and the empty slot goes");
    }

    #[test]
    fn a_large_scene_followed_by_small_ones_gives_slots_and_buffers_back() {
        let gpu = crate::gpu::Gpu::open_headless(crate::gpu::HeadlessOptions {
            adapter: crate::gpu::AdapterPreference::Software,
        })
        .unwrap();
        let (mut pool, meshes, _, one) = warmed_pool(&gpu);
        let per_slot = pool.bytes();
        let mut large = one.clone();
        let instance = one.instances[0];
        large.instances = vec![instance; 40];
        large.morph_weights = vec![0.5];
        pool.prepare(&gpu.device, &gpu.queue, &meshes, &large, |_| true).unwrap();
        assert_eq!((pool.slots.len(), pool.bytes()), (40, 40 * per_slot));
        // Only the first instance morphs from now on: the tail idles for IDLE_FRAMES, then goes, once.
        for _ in 0..=IDLE_FRAMES + 1 {
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &one, |_| true).unwrap();
        }
        assert_eq!((pool.slots.len(), pool.bytes()), (1, per_slot), "back to the morphing count");
        let empty = Scene::new(omuzaki_render_api::Camera::default());
        for _ in 0..=IDLE_FRAMES {
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &empty, |_| true).unwrap();
        }
        assert_eq!((pool.slots.len(), pool.bytes(), pool.flags.capacity()), (0, 0, 0), "and to nothing");
    }

    #[test]
    fn a_scripted_run_reuses_every_buffer_and_ends_with_nothing_held_on_the_cpu_path() {
        scripted_run(false);
    }

    #[test]
    fn a_scripted_run_reuses_every_buffer_and_ends_with_nothing_held_on_the_gpu_path() {
        scripted_run(true);
    }

    /// The scripted scenes through `MorphPool::prepare` twice, on the GPU path or the CPU path.
    fn scripted_run(on_gpu: bool) {
        use omuzaki_core::glam::Mat4;
        use omuzaki_render_api::{Camera, MaterialId, MeshId, MeshOptions, MorphRange};

        let gpu = crate::gpu::Gpu::open_headless(crate::gpu::HeadlessOptions {
            adapter: crate::gpu::AdapterPreference::Software,
        })
        .unwrap();
        let mut meshes = Arena::default();
        let mut pool = if on_gpu { MorphPool::new(&gpu.device) } else { MorphPool::default() };
        assert_eq!(pool.path(), if on_gpu { MorphPath::Gpu } else { MorphPath::Cpu });
        let ids: Vec<MeshId> = [
            omuzaki_render_mock::shapes::cuboid(Vec3::ONE),
            omuzaki_render_mock::shapes::quad(omuzaki_core::glam::Vec2::ONE),
        ]
        .iter()
        .map(|mesh| {
            let n = mesh.positions.len();
            let deltas = vec![Vec3::X; n];
            let set = MorphSet::from_dense(n, &[(Slug::new("a").unwrap(), &deltas, &[])], 0.0).unwrap();
            let options = MeshOptions { morphs: Some(set), ..MeshOptions::default() };
            let (index, generation) = meshes.insert(GpuMesh::upload(&gpu, mesh, &options).unwrap());
            pool.attach(&gpu.device, index, generation, meshes.get(index, generation).unwrap()).unwrap();
            MeshId::from_parts(index, generation)
        })
        .collect();
        // Growing and shrinking scenes, zero weights in the middle and at the end, two meshes, two materials (the
        // odd one is outlined).
        let scenes: Vec<Scene> = (0..20_usize)
            .map(|k| {
                let mut scene = Scene::new(Camera::default());
                for i in 0..[1, 8, 3, 12, 2, 6, 12, 1, 5, 9][k % 10] {
                    let material = MaterialId::from_parts(((i * 3 + k) % 2) as u32, 0);
                    let mut instance = Instance::new(ids[(i + k) % 2], material, Mat4::IDENTITY);
                    instance.morph = Some(MorphRange { start: i as u32, len: 1 });
                    scene.instances.push(instance);
                    let zero = (i * 7 + k).is_multiple_of(3) || (k.is_multiple_of(2) && i == scene.instances.len() - 1);
                    scene.morph_weights.push(if zero { 0.0 } else { 0.25 });
                }
                scene
            })
            .collect();
        let outlined = |instance: &Instance| instance.material.index() == 1;
        let held = |pool: &MorphPool| -> Vec<_> {
            pool.slots
                .iter()
                .map(|s| (s.shapes.buffer.clone(), s.smooth.buffer.clone(), s.gpu.as_ref().map(|g| g.params.clone())))
                .collect()
        };
        for scene in &scenes {
            pool.prepare(&gpu.device, &gpu.queue, &meshes, scene, outlined).unwrap();
        }
        let warmed = held(&pool);
        // Everything that can allocate: the vectors' capacities, which must not change once warmed.
        let capacities = |pool: &MorphPool| -> (Vec<usize>, [usize; 5]) {
            let per_slot = pool
                .slots
                .iter()
                .flat_map(|s| [s.blended.capacity(), s.smooth_cpu.capacity(), s.weights.capacity()])
                .collect();
            let pool_wide = [
                pool.slots.capacity(),
                pool.flags.capacity(),
                pool.sums.capacity(),
                pool.pending.capacity(),
                pool.meshes_gpu.capacity(),
            ];
            (per_slot, pool_wide)
        };
        for scene in &scenes {
            let before = capacities(&pool);
            pool.prepare(&gpu.device, &gpu.queue, &meshes, scene, outlined).unwrap();
            assert_eq!(capacities(&pool), before, "a capacity changed in the second pass");
            for (slot, was) in pool.slots.iter().zip(&warmed) {
                let now = (&slot.shapes.buffer, &slot.smooth.buffer, slot.gpu.as_ref().map(|g| &g.params));
                assert!(now.0 == &was.0 && now.1 == &was.1 && now.2 == was.2.as_ref(), "a buffer was replaced");
            }
        }
        let empty = Scene::new(Camera::default());
        for _ in 0..=IDLE_FRAMES {
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &empty, outlined).unwrap();
        }
        // What is left is the meshes' own GPU data (nothing on the CPU path), which lives as long as the meshes.
        let mesh_data: u64 = pool.meshes_gpu.iter().flatten().map(|(_, data)| data.bytes).sum();
        assert_eq!(mesh_data > 0, on_gpu);
        let kept = (
            pool.slots.len(),
            pool.bytes() - mesh_data,
            pool.flags.capacity(),
            pool.sums.capacity(),
            pool.pending.capacity(),
        );
        assert_eq!(kept, (0, 0, 0, 0, 0), "slots, buffers, scratch and the dispatch list are all given back");
        let attached = pool.meshes_gpu.len();
        assert_eq!(attached > 0, on_gpu);
        // With the pool empty, instances whose weights are all zero need no slot: nothing is created, let alone
        // created and dropped again every frame.
        for scene in &scenes {
            let mut zeroed = scene.clone();
            zeroed.morph_weights.iter_mut().for_each(|w| *w = 0.0);
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &zeroed, outlined).unwrap();
            assert_eq!((pool.slots.len(), pool.slots.capacity()), (0, 0), "zero weights made a slot");
        }
        // Releasing the meshes frees their data and the table that held it.
        for id in &ids {
            pool.forget(id.index(), id.generation());
        }
        assert_eq!((pool.bytes(), pool.meshes_gpu.len(), pool.meshes_gpu.capacity()), (0, 0, 0));
    }
}
