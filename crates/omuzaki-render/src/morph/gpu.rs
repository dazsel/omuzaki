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
/// The storage buffers one pipeline binds, across both of its groups; the device must allow this many.
const STORAGE_BUFFERS: u32 = 5;
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
    let limits = device.limits();
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

/// A storage buffer holding `data` (at least 16 bytes, which a binding needs even for an empty array).
fn upload<T: Pod>(device: &wgpu::Device, label: &str, data: &[T], bytes: &mut u64) -> wgpu::Buffer {
    let mut raw: Vec<u8> = bytemuck::cast_slice(data).to_vec();
    raw.resize(raw.len().max(16), 0);
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
        let (Some(slot), Some(mesh)) = (slot(p.slot), mesh(p.mesh)) else { continue };
        let (pipeline, data, instance) = match p.step {
            Step::Blend => (&pipelines.blend, &mesh.blend, slot.blend.as_ref()),
            Step::Weld => (&pipelines.weld, &mesh.weld, slot.weld.as_ref()),
        };
        let Some(instance) = instance else { continue };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, data, &[]);
        pass.set_bind_group(1, instance, &[]);
        pass.dispatch_workgroups(mesh.vertices.div_ceil(WORKGROUP).max(1), 1, 1);
    }
}
