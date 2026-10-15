//! The GPU path: the blend of `cpu.rs` as a compute pass (`shader/morph.wgsl`), writing into the same pooled
//! per-instance buffers.
//!
//! A mesh's sparse targets are regrouped by vertex and uploaded once, when the mesh is created ([`MeshGpu`]); a frame
//! uploads only the weights. Nothing is read back: the instance's bounds are the mesh's, grown by what the weights
//! can move a vertex ([`grow`]).

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::Vec3;
use omuzaki_render_api::RenderError;
use wgpu::util::DeviceExt;

use super::MAX_TARGETS;
use super::MorphInput;

/// Threads in a workgroup (`@workgroup_size` in the shader).
const WORKGROUP: u32 = 64;
/// Workgroups one dispatch may span on one axis, the WebGPU guarantee.
const MAX_WORKGROUPS: u32 = 65_535;
/// The storage buffers one pipeline binds, across both of its groups; the device must allow this many. The weld
/// pipeline binds the most: `slot_of`, `group_first` and `members`, then `params`, `blended` and `directions`.
const STORAGE_BUFFERS: u32 = 6;
/// Bytes before the weights in the params buffer: the vertex count, the target count and padding.
pub(super) const PARAMS_HEADER: u64 = 16;

/// One sparse entry regrouped by vertex; the layout of `Entry` in the shader.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Entry {
    target: u32,
    has_normal: u32,
    position: [f32; 3],
    normal: [f32; 3],
}

/// Whether `device` can run the blend: compute with enough storage buffers. A WebGL2 device has none.
pub(super) fn supported(device: &wgpu::Device) -> bool {
    supported_by(&device.limits())
}

/// Whether a device with `limits` can run the blend.
fn supported_by(limits: &wgpu::Limits) -> bool {
    limits.max_storage_buffers_per_shader_stage >= STORAGE_BUFFERS
        && limits.max_compute_workgroup_size_x >= WORKGROUP
        && limits.max_compute_invocations_per_workgroup >= WORKGROUP
}

/// The two compute pipelines and the layouts of their bind groups.
pub(super) struct Pipelines {
    blend: wgpu::ComputePipeline,
    weld: wgpu::ComputePipeline,
    blend_data: wgpu::BindGroupLayout,
    weld_data: wgpu::BindGroupLayout,
    blend_slot: wgpu::BindGroupLayout,
    weld_slot: wgpu::BindGroupLayout,
}

fn storage(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl Pipelines {
    /// Compiles the shader and builds both pipelines.
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("morph"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shader/morph.wgsl").into()),
        });
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries })
        };
        let blend_data = layout("morph blend data", &[storage(0, true), storage(1, true), storage(2, true)]);
        let weld_data = layout("morph weld data", &[storage(3, true), storage(4, true), storage(5, true)]);
        let blend_slot = layout("morph blend instance", &[storage(0, false), storage(1, true)]);
        let weld_slot = layout("morph weld instance", &[storage(1, true), storage(2, true), storage(3, false)]);
        let pipeline = |label, data, slot, entry_point| {
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(data), Some(slot)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        Self {
            blend: pipeline("morph blend", &blend_data, &blend_slot, "blend"),
            weld: pipeline("morph weld", &weld_data, &weld_slot, "weld"),
            blend_data,
            weld_data,
            blend_slot,
            weld_slot,
        }
    }
}

/// A mesh's morph data on the GPU, and what the CPU keeps of it.
pub(super) struct MeshGpu {
    blend: wgpu::BindGroup,
    weld: wgpu::BindGroup,
    /// Per target, how far it can move any vertex per unit weight on each axis: up (the largest positive delta) and
    /// down (the largest negative one, as a positive number).
    extents: Vec<Extent>,
    /// Per target, how far the centre of the mesh's box moves at weight 1 (exact for a rigid translation): what the
    /// transparent sort follows.
    shifts: Vec<Vec3>,
    /// The vertices the mesh has.
    pub(super) vertices: u32,
    /// What the uploaded buffers hold, for the stats.
    pub(super) bytes: u64,
}

/// How far a target moves vertices on each axis per unit weight.
#[derive(Copy, Clone, Default)]
struct Extent {
    up: Vec3,
    down: Vec3,
}

/// A storage buffer holding `data`, padded to at least one element (and 16 bytes): a binding to a runtime-sized array
/// must be big enough for one, even when the array is empty.
fn upload<T: Pod>(device: &wgpu::Device, label: &str, data: &[T], bytes: &mut u64) -> wgpu::Buffer {
    let mut raw: Vec<u8> = bytemuck::cast_slice(data).to_vec();
    raw.resize(raw.len().max(size_of::<T>()).max(16), 0);
    *bytes += raw.len() as u64;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: &raw,
        usage: wgpu::BufferUsages::STORAGE,
    })
}

impl MeshGpu {
    /// Uploads the mesh's targets and weld table; `base` is the mesh's own shape stream (the buffer the targets apply
    /// to, which a dynamic mesh rewrites).
    ///
    /// # Errors
    /// `LimitExceeded` if the mesh is too big for one dispatch or a buffer is over the device's binding size.
    pub(super) fn new(
        device: &wgpu::Device,
        pipelines: &Pipelines,
        base: &wgpu::Buffer,
        input: &MorphInput<'_>,
    ) -> Result<Self, RenderError> {
        let vertices = input.base.len();
        let groups = u64::from(MAX_WORKGROUPS) * u64::from(WORKGROUP);
        let vertices_u32 = u32::try_from(vertices)
            .ok()
            .filter(|_| vertices as u64 <= groups)
            .ok_or(RenderError::LimitExceeded { what: "vertices in a morphed mesh", limit: groups })?;

        // Entries grouped by vertex, each vertex's in target order (the order the CPU sums in).
        let mut first = vec![0_u32; vertices + 1];
        for target in input.set.targets() {
            for &index in &target.indices {
                if let Some(count) = first.get_mut(index as usize + 1) {
                    *count += 1;
                }
            }
        }
        for v in 0..vertices {
            first[v + 1] += first[v];
        }
        let mut cursor = first.clone();
        let total = first.last().copied().unwrap_or(0) as usize;
        let mut entries = vec![Entry::zeroed(); total];
        let mut extents = Vec::with_capacity(input.set.len());
        let mut shifts = Vec::with_capacity(input.set.len());
        for (t, target) in input.set.targets().iter().enumerate() {
            let mut extent = Extent::default();
            for (k, &index) in target.indices.iter().enumerate() {
                let (Some(at), Some(&position)) = (cursor.get_mut(index as usize), target.positions.get(k)) else {
                    debug_assert!(false, "morph target {:?}: entry {k} does not fit the mesh", target.name);
                    continue;
                };
                let normal = target.normals.get(k).copied();
                if let Some(entry) = entries.get_mut(*at as usize) {
                    *entry = Entry {
                        target: u32::try_from(t).unwrap_or(u32::MAX),
                        has_normal: u32::from(normal.is_some()),
                        position: position.to_array(),
                        normal: normal.unwrap_or(Vec3::ZERO).to_array(),
                    };
                }
                *at += 1;
                extent = Extent { up: extent.up.max(position), down: extent.down.max(-position) };
            }
            extents.push(extent);
            shifts.push(centre_shift(input.base, target));
        }

        // The weld table by point: the vertices at each, in vertex order (the order `resmooth` sums in).
        let mut group_first = vec![0_u32; input.weld_count + 1];
        for &point in input.welds {
            if let Some(count) = group_first.get_mut(point + 1) {
                *count += 1;
            }
        }
        for p in 0..input.weld_count {
            group_first[p + 1] += group_first[p];
        }
        let mut next = group_first.clone();
        let mut members = vec![0_u32; vertices];
        for (v, &point) in input.welds.iter().enumerate() {
            if let Some(at) = next.get_mut(point) {
                if let Some(member) = members.get_mut(*at as usize) {
                    *member = u32::try_from(v).unwrap_or(u32::MAX);
                }
                *at += 1;
            }
        }
        let slot_of: Vec<u32> = input.welds.iter().map(|&p| u32::try_from(p).unwrap_or(u32::MAX)).collect();

        let mut bytes = 0;
        let buffers = [
            upload(device, "morph first", &first, &mut bytes),
            upload(device, "morph entries", &entries, &mut bytes),
            upload(device, "morph slot_of", &slot_of, &mut bytes),
            upload(device, "morph group_first", &group_first, &mut bytes),
            upload(device, "morph members", &members, &mut bytes),
        ];
        let largest = device.limits().max_storage_buffer_binding_size;
        if let Some(too_big) = buffers.iter().map(wgpu::Buffer::size).find(|size| *size > largest) {
            return Err(RenderError::LimitExceeded {
                what: "bytes of morph data in one buffer",
                limit: largest.min(too_big),
            });
        }
        let [first, entries, slot_of, group_first, members] = buffers;
        let group = |label, layout, entries: &[(u32, &wgpu::Buffer)]| {
            let entries: Vec<_> = entries
                .iter()
                .map(|(binding, buffer)| wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: buffer.as_entire_binding(),
                })
                .collect();
            device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(label), layout, entries: &entries })
        };
        Ok(Self {
            blend: group("morph blend data", &pipelines.blend_data, &[(0, base), (1, &first), (2, &entries)]),
            weld: group("morph weld data", &pipelines.weld_data, &[(3, &slot_of), (4, &group_first), (5, &members)]),
            extents,
            shifts,
            vertices: vertices_u32,
            bytes,
        })
    }
}

/// How far the weights can move any vertex of the mesh on each axis, as `(down, up)`: the sums over the targets of
/// each one's weight times the largest delta it has that way (a negative weight swaps a target's two). A vertex moves
/// by at most this, however the targets overlap, so the mesh's bounds grown by it contain the blended mesh's.
pub(super) fn grow(weights: &[f32], mesh: &MeshGpu) -> (Vec3, Vec3) {
    let (mut down, mut up) = (Vec3::ZERO, Vec3::ZERO);
    for (weight, extent) in weights.iter().zip(&mesh.extents) {
        let (towards_up, towards_down) =
            if *weight >= 0.0 { (extent.up, extent.down) } else { (extent.down, extent.up) };
        up += towards_up * weight.abs();
        down += towards_down * weight.abs();
    }
    (down, up)
}

/// How far the centre of the vertices' box moves when `target` is applied alone at weight 1.
fn centre_shift(base: &[crate::resource::mesh::Shape], target: &omuzaki_render_api::SparseMorph) -> Vec3 {
    let (mut before, mut after) = (
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
    );
    let mut moved = target.indices.iter().zip(&target.positions).peekable();
    for (v, shape) in base.iter().enumerate() {
        let at = Vec3::from(shape.position);
        let to = match moved.next_if(|(index, _)| **index as usize == v) {
            Some((_, delta)) => at + *delta,
            None => at,
        };
        before = (before.0.min(at), before.1.max(at));
        after = (after.0.min(to), after.1.max(to));
    }
    if base.is_empty() { Vec3::ZERO } else { ((after.0 + after.1) - (before.0 + before.1)) * 0.5 }
}

/// Where the weights move the centre of the mesh's box: the sum of each target's shift times its weight.
pub(super) fn shift(weights: &[f32], mesh: &MeshGpu) -> Vec3 {
    weights.iter().zip(&mesh.shifts).map(|(weight, shift)| *shift * *weight).sum()
}

/// The per-instance side of the GPU path: the params buffer and the bind groups over the instance's streams.
pub(super) struct SlotGpu {
    pub(super) params: wgpu::Buffer,
    blend: Option<wgpu::BindGroup>,
    weld: Option<wgpu::BindGroup>,
}

impl SlotGpu {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let size = PARAMS_HEADER + 4 * u64::from(MAX_TARGETS);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("morph params"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { params, blend: None, weld: None }
    }

    /// Uploads this blend's parameters: the vertex count, the target count and one weight per target.
    pub(super) fn write_params(&self, queue: &wgpu::Queue, vertices: u32, weights: &[f32]) {
        let targets = u32::try_from(weights.len()).unwrap_or(u32::MAX);
        queue.write_buffer(&self.params, 0, bytemuck::cast_slice(&[vertices, targets, 0, 0]));
        queue.write_buffer(&self.params, PARAMS_HEADER, bytemuck::cast_slice(weights));
    }

    /// The streams were replaced: the bind groups over them are stale.
    pub(super) fn forget_groups(&mut self) {
        self.blend = None;
        self.weld = None;
    }

    pub(super) fn forget_weld(&mut self) {
        self.weld = None;
    }

    /// Builds the blend bind group over `shapes` if it is missing.
    pub(super) fn bind_blend(&mut self, device: &wgpu::Device, pipelines: &Pipelines, shapes: &wgpu::Buffer) {
        if self.blend.is_none() {
            self.blend = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("morph blend instance"),
                layout: &pipelines.blend_slot,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: shapes.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.params.as_entire_binding() },
                ],
            }));
        }
    }

    /// Builds the weld bind group over `shapes` and `smooth` if it is missing.
    pub(super) fn bind_weld(
        &mut self,
        device: &wgpu::Device,
        pipelines: &Pipelines,
        shapes: &wgpu::Buffer,
        smooth: &wgpu::Buffer,
    ) {
        if self.weld.is_none() {
            self.weld = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("morph weld instance"),
                layout: &pipelines.weld_slot,
                entries: &[
                    wgpu::BindGroupEntry { binding: 1, resource: self.params.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: shapes.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: smooth.as_entire_binding() },
                ],
            }));
        }
    }
}

/// Which of the two dispatches an instance needs.
#[derive(Copy, Clone)]
pub(super) enum Step {
    Blend,
    Weld,
}

/// Where an instance's dispatches go: the pipelines, the mesh's data and the slot and mesh indexes `record` finds
/// them by.
pub(super) struct Target<'a> {
    pub(super) pipelines: &'a Pipelines,
    pub(super) data: &'a MeshGpu,
    pub(super) mesh: usize,
    pub(super) slot: usize,
}

impl Target<'_> {
    /// The dispatch of `step` for this instance.
    pub(super) fn pending(&self, step: Step) -> Pending {
        Pending { slot: self.slot, mesh: self.mesh, step }
    }
}

/// A dispatch to record this frame.
#[derive(Copy, Clone)]
pub(super) struct Pending {
    pub(super) slot: usize,
    pub(super) mesh: usize,
    pub(super) step: Step,
}

/// Records `pending` into one compute pass, in order (a weld after its blend); wgpu orders the dispatches' writes
/// and reads. `slot_of` and `mesh_of` find the instance's and the mesh's side.
pub(super) fn record<'a>(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &Pipelines,
    pending: &[Pending],
    slot: impl Fn(usize) -> Option<&'a SlotGpu>,
    mesh: impl Fn(usize) -> Option<&'a MeshGpu>,
) {
    let mut pass =
        encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("morph"), timestamp_writes: None });
    for p in pending {
        let (Some(slot), Some(mesh)) = (slot(p.slot), mesh(p.mesh)) else {
            // A pending dispatch always has its slot and mesh: `prepare` queued it this frame from both.
            debug_assert!(false, "a pending morph dispatch lost its slot or its mesh");
            continue;
        };
        let (pipeline, data, instance) = match p.step {
            Step::Blend => (&pipelines.blend, &mesh.blend, slot.blend.as_ref()),
            Step::Weld => (&pipelines.weld, &mesh.weld, slot.weld.as_ref()),
        };
        let Some(instance) = instance else {
            // `prepare` builds the bind group before it queues the dispatch.
            debug_assert!(false, "a pending morph dispatch has no bind group");
            continue;
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, data, &[]);
        pass.set_bind_group(1, instance, &[]);
        pass.dispatch_workgroups(mesh.vertices.div_ceil(WORKGROUP).max(1), 1, 1);
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss, clippy::too_many_lines)]
mod tests {
    use omuzaki_core::glam::Mat4;
    use omuzaki_core::glam::Vec3;
    use omuzaki_core::id::Slug;
    use omuzaki_render_api::{
        Camera, Instance, MaterialId, MeshData, MeshId, MeshOptions, MorphRange, MorphSet, Scene, SparseMorph,
    };

    use super::super::MorphPool;
    use super::supported_by;
    use crate::arena::Arena;
    use crate::gpu::{AdapterPreference, Gpu, HeadlessOptions, WAIT_LIMIT};
    use crate::resource::mesh::GpuMesh;

    /// A deterministic stream of floats in `-1..1` (a small LCG; nothing here may depend on a platform's `rand`).
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 40) as f32 / (1_u64 << 23) as f32) - 1.0
        }

        fn vec3(&mut self) -> Vec3 {
            Vec3::new(self.next(), self.next(), self.next())
        }
    }

    /// The synthetic head of `docs/design/byom.md` D2: random positions and normals, skinned to four joints, with every
    /// fourth vertex a copy of the one before at the same point (a seam, so the weld table has groups), and 52 targets,
    /// each a contiguous region of 3 to 25 % of the head with one vertex in eight skipped, normal deltas on the odd ones.
    fn head(n: usize, targets: usize) -> (MeshData, MorphSet) {
        let mut noise = Noise(0x6d6f_7270);
        let mut mesh = MeshData::default();
        for i in 0..n {
            if i % 4 == 3 {
                let previous = mesh.positions[i - 1];
                mesh.positions.push(previous);
            } else {
                mesh.positions.push(noise.vec3() * 0.15 + Vec3::new(0.0, 1.6, 0.0));
            }
            mesh.normals.push(noise.vec3().normalize_or(Vec3::Z));
            mesh.joints.push([(i % 4) as u16, 0, 0, 0]);
            mesh.weights.push([1.0, 0.0, 0.0, 0.0]);
        }
        mesh.indices = (0..n as u32 - 2).step_by(3).flat_map(|i| [i, i + 1, i + 2]).collect();
        let sparse = (0..targets)
            .map(|t| {
                let share = 0.03 + 0.22 * (t % 12) as f32 / 11.0;
                let len = (n as f32 * share) as usize;
                let start = (t * 7_919) % (n - len);
                let indices: Vec<u32> = (start..start + len).filter(|i| i % 8 != 0).map(|i| i as u32).collect();
                let positions = indices.iter().map(|_| noise.vec3() * 0.01).collect();
                let normals =
                    if t % 2 == 1 { indices.iter().map(|_| noise.vec3() * 0.2).collect() } else { Vec::new() };
                SparseMorph { name: Slug::new(format!("t{t}")).unwrap(), indices, positions, normals }
            })
            .collect();
        (mesh, MorphSet::from_targets(n, sparse).unwrap())
    }

    fn open() -> Gpu {
        Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap()
    }

    /// Records `pool`'s dispatches, then reads back the first `bytes` of each of `buffers`.
    fn run_and_read(gpu: &Gpu, pool: &MorphPool, buffers: &[(&wgpu::Buffer, u64)]) -> Vec<Vec<f32>> {
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        pool.record(&mut encoder);
        let staging: Vec<wgpu::Buffer> = buffers
            .iter()
            .map(|(buffer, bytes)| {
                let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("morph test read-back"),
                    size: *bytes,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, *bytes);
                staging
            })
            .collect();
        gpu.queue.submit([encoder.finish()]);
        for staging in &staging {
            staging.slice(..).map_async(wgpu::MapMode::Read, |result| result.unwrap());
        }
        gpu.wait_idle(WAIT_LIMIT).unwrap();
        staging.iter().map(|s| bytemuck::cast_slice(&s.slice(..).get_mapped_range().unwrap()).to_vec()).collect()
    }

    /// One scene with one morphing instance of `mesh`, weights `weights`.
    fn scene_of(mesh: MeshId, weights: &[f32]) -> Scene {
        let mut scene = Scene::new(Camera::default());
        let mut instance = Instance::new(mesh, MaterialId::from_parts(0, 0), Mat4::IDENTITY);
        instance.morph = Some(MorphRange { start: 0, len: weights.len() as u32 });
        scene.instances.push(instance);
        scene.morph_weights = weights.to_vec();
        scene
    }

    /// Both paths blend the 20 000-vertex, 52-target head at the weights of `docs/design/byom.md` D2's check (0, 0.25,
    /// 0.5 and 1 on every target) and at mixed and negative ones; the GPU result is within 1e-4 m of the CPU's, its
    /// outline directions are within 1e-4, and its bounds contain the CPU path's exact ones.
    #[test]
    fn the_gpu_path_matches_the_cpu_path_on_a_20_000_vertex_52_target_head() {
        let gpu = open();
        let (mesh, set) = head(20_000, 52);
        let n = mesh.positions.len();
        let mut meshes = Arena::default();
        let upload =
            GpuMesh::upload(&gpu, &mesh, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }, true).unwrap();
        let (index, generation) = meshes.insert(upload);
        let id = MeshId::from_parts(index, generation);
        let (mut gpu_pool, mut cpu_pool) = (MorphPool::new(&gpu.device), MorphPool::default());
        assert_eq!(gpu_pool.path().name(), "gpu", "this adapter has compute");
        gpu_pool.attach(&gpu.device, index, generation, meshes.get(index, generation).unwrap()).unwrap();

        let mut noise = Noise(7);
        let mixed: Vec<f32> = (0..52).map(|_| noise.next() * 1.5).collect();
        let cases: [(&str, Vec<f32>); 6] = [
            ("0", vec![0.0; 52]),
            ("0.25", vec![0.25; 52]),
            ("0.5", vec![0.5; 52]),
            ("1", vec![1.0; 52]),
            ("-1", vec![-1.0; 52]),
            ("mixed in -1.5..1.5", mixed),
        ];
        let (mut worst_position, mut worst_normal, mut worst_direction) = (0.0_f32, 0.0_f32, 0.0_f32);
        for (name, weights) in &cases {
            let scene = scene_of(id, weights);
            let outlined = |_: &Instance| true;
            cpu_pool.prepare(&gpu.device, &gpu.queue, &meshes, &scene, outlined).unwrap();
            gpu_pool.prepare(&gpu.device, &gpu.queue, &meshes, &scene, outlined).unwrap();
            if weights.iter().all(|w| *w == 0.0) {
                assert!(
                    gpu_pool.shapes(0).is_none() && cpu_pool.shapes(0).is_none(),
                    "zero weights draw the mesh's own stream"
                );
                continue;
            }
            let slot = &gpu_pool.slots[0];
            let (shapes, smooth) = (slot.shapes.buffer.as_ref().unwrap(), slot.smooth.buffer.as_ref().unwrap());
            let [blended, directions] =
                &run_and_read(&gpu, &gpu_pool, &[(shapes, (n * 24) as u64), (smooth, (n * 12) as u64)])[..]
            else {
                unreachable!("two buffers were read")
            };
            let reference = &cpu_pool.slots[0];
            let (mut position_error, mut normal_error, mut direction_error) = (0.0_f32, 0.0_f32, 0.0_f32);
            for (v, shape) in reference.blended.iter().enumerate() {
                for axis in 0..3 {
                    position_error = position_error.max((blended[v * 6 + axis] - shape.position[axis]).abs());
                    normal_error = normal_error.max((blended[v * 6 + 3 + axis] - shape.normal[axis]).abs());
                    direction_error =
                        direction_error.max((directions[v * 3 + axis] - reference.smooth_cpu[v][axis]).abs());
                }
            }
            println!(
                "weights {name}: max position error {position_error:e} m, normal {normal_error:e}, outline direction {direction_error:e}"
            );
            (worst_position, worst_normal, worst_direction) = (
                worst_position.max(position_error),
                worst_normal.max(normal_error),
                worst_direction.max(direction_error),
            );

            // The conservative bounds contain the exact ones, whole and per joint.
            let (exact, grown) = (&reference.bounds, &slot.bounds);
            let palette = [Mat4::IDENTITY; 4];
            for palette in [None, Some(&palette[..])] {
                let (e, g) = (exact.instance(&Mat4::IDENTITY, palette), grown.instance(&Mat4::IDENTITY, palette));
                assert!(
                    g.min.cmple(e.min).all() && g.max.cmpge(e.max).all(),
                    "weights {name}: {g:?} must contain {e:?}"
                );
            }
        }
        println!(
            "worst over every case: position {worst_position:e} m, normal {worst_normal:e}, outline {worst_direction:e}"
        );
        assert!(worst_position <= 1e-4, "position error {worst_position}");
        assert!(worst_normal <= 1e-4, "normal error {worst_normal}");
        assert!(worst_direction <= 1e-4, "outline direction error {worst_direction}");
    }

    /// A single triangle whose vertex 0's normal all but cancels under the target: `(0, 0, 1)` plus `(1e-16, 0, -1)`.
    #[test]
    fn a_normal_that_all_but_cancels_still_normalises_as_on_the_cpu() {
        let gpu = open();
        let mesh = MeshData {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            normals: vec![Vec3::Z; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        };
        let target = SparseMorph {
            name: Slug::new("cancel").unwrap(),
            indices: vec![0],
            positions: vec![Vec3::ZERO],
            normals: vec![Vec3::new(1e-16, 0.0, -1.0)],
        };
        let set = MorphSet::from_targets(3, vec![target]).unwrap();
        let mut meshes = Arena::default();
        let upload =
            GpuMesh::upload(&gpu, &mesh, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }, true).unwrap();
        let (index, generation) = meshes.insert(upload);
        let (mut gpu_pool, mut cpu_pool) = (MorphPool::new(&gpu.device), MorphPool::default());
        gpu_pool.attach(&gpu.device, index, generation, meshes.get(index, generation).unwrap()).unwrap();
        let scene = scene_of(MeshId::from_parts(index, generation), &[1.0]);
        for pool in [&mut gpu_pool, &mut cpu_pool] {
            pool.prepare(&gpu.device, &gpu.queue, &meshes, &scene, |_| false).unwrap();
        }
        let shapes = gpu_pool.slots[0].shapes.buffer.as_ref().unwrap();
        let [blended] = &run_and_read(&gpu, &gpu_pool, &[(shapes, 3 * 24)])[..] else { unreachable!("one buffer") };
        let cpu_normal = Vec3::from(cpu_pool.slots[0].blended[0].normal);
        assert!((cpu_normal - Vec3::X).length() < 1e-4, "the CPU normalises the sliver to +x: {cpu_normal}");
        let gpu_normal = Vec3::new(blended[3], blended[4], blended[5]);
        assert!((gpu_normal - cpu_normal).length() < 1e-4, "the GPU agrees: {gpu_normal} against {cpu_normal}");
    }

    #[test]
    fn the_gpu_path_needs_six_storage_buffers() {
        let limits = |n| wgpu::Limits { max_storage_buffers_per_shader_stage: n, ..wgpu::Limits::defaults() };
        assert!(!supported_by(&limits(5)), "the weld pipeline binds six");
        assert!(supported_by(&limits(6)));
        assert!(!supported_by(&wgpu::Limits::downlevel_webgl2_defaults()), "WebGL2 has no storage buffers");
    }
}
