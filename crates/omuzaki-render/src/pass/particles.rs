//! The `Particles` pass (particles.md D4 to D6): the scene's spawn batches as camera-facing, velocity-stretched or
//! tumbling sprites, drawn with coverage instead of blending between `Outlines` and `Transparent`.
//!
//! **Data.** Each frame the batches are grouped by system (scene order within a system) and packed into blocks: a
//! system's 256-byte header ([`SystemUniform`], with the frame's density and the block's batch count filled in), then up
//! to [`BLOCK_BATCHES`] batch records of 176 bytes. Blocks sit at aligned offsets in one uniform buffer, sized at
//! creation for `MAX_SPAWN_BATCHES` and never grown, and are bound with a dynamic offset beside the frame uniform (whose
//! round is the other dynamic offset). Nothing is per particle, and packing allocates nothing: the staging bytes, the
//! sort order and the block list are kept.
//!
//! **Draw.** Two instanced draws per block, `draw(0..4, 0..slots)`: the colour draw (alpha-to-coverage, depth test and
//! write, RGB) and the alpha draw (depth `Equal`, alpha only), as `shader/particles.wgsl` explains. At one sample the
//! colour draw is a 0.5 alpha test. No compute and no vertex-stage storage buffers, so every capability profile draws
//! with the same shader. The shape, orientation and sprite kind are uniform branches, so pipelines vary only by sample
//! count.
//!
//! **The reference.** The vertex stage evaluates `shader/particles_state.wgsl`, render-api's `particles::evaluate`
//! restated; the parity test below runs it in a compute shader (tests only) against the Rust.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{Pass, RenderError, Scene, SpawnBatch, limits};

use super::{FramePass, Outcome, PassCx};
use crate::arena::Arena;
use crate::frame::draws::FrameUniforms;
use crate::gpu::{DEPTH_FORMAT, SCENE_FORMAT};
use crate::program::opaque_source;
use crate::resource::particles::{GpuParticleSystem, SystemUniform};
use crate::shared::Shared;

/// Batch records per block: as many as fit WebGL2's 16 KiB uniform block after the header.
pub(crate) const BLOCK_BATCHES: usize = 91;
const HEADER_BYTES: usize = size_of::<SystemUniform>();
const BATCH_BYTES: usize = size_of::<BatchUniform>();
/// The bytes a block binding covers.
const BLOCK_BYTES: usize = HEADER_BYTES + BLOCK_BATCHES * BATCH_BYTES;
/// The block buffer: every batch in a block of its own at the worst alignment (256), plus one whole block's binding.
const BUFFER_BYTES: usize = limits::MAX_SPAWN_BATCHES * (HEADER_BYTES + BATCH_BYTES + 255) + BLOCK_BYTES;

const _: () = assert!(BATCH_BYTES == 176 && BLOCK_BYTES <= 16_384, "a block fits WebGL2's uniform block");

/// One spawn batch as the shader reads it (`struct Batch` in `shader/particles_state.wgsl`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BatchUniform {
    emitter: [[f32; 4]; 4],
    /// The anchor's velocity and the interval between births.
    velocity: [f32; 4],
    /// `EmitterShape::extent` and `age0 = time - start`, subtracted in f64 (D3).
    extent: [f32; 4],
    /// First, count, seed, and the emitter shape's code with the colour count at bit 4 and the cell count at bit 8.
    ids: [u32; 4],
    /// The block's first slot for this batch and the scale's bits.
    misc: [u32; 4],
    /// Up to eight listed cells, two per word.
    cells: [u32; 4],
    /// Up to eight sRGB colours, RGBA8 with red in the low byte.
    colours: [u32; 8],
}

/// `batch` packed for the frame at `time`, its slots starting at `slot` in its block. The colours are the batch's own
/// or else its system's.
fn batch_uniform(batch: &SpawnBatch, system: &GpuParticleSystem, time: Seconds, slot: u32) -> BatchUniform {
    let colours = batch.colours.as_ref().unwrap_or(&system.desc.params.colours).as_slice();
    let mut packed_colours = [0; 8];
    for (word, colour) in packed_colours.iter_mut().zip(colours) {
        *word = u32::from_le_bytes([colour.r, colour.g, colour.b, colour.a]);
    }
    let listed = batch.cells.as_slice();
    let mut cells = [0; 4];
    for (k, &cell) in listed.iter().enumerate().take(8) {
        cells[k / 2] |= u32::from(cell) << (16 * (k % 2));
    }
    // At most eight of each.
    #[allow(clippy::cast_possible_truncation)]
    let (colour_count, cell_count) = (colours.len().min(8) as u32, listed.len().min(8) as u32);
    let extent = batch.shape.extent();
    // The long subtraction in f64, then f32 for everything that follows, as `evaluate` does.
    #[allow(clippy::cast_possible_truncation)]
    let age0 = (time.0 - batch.start.0) as f32;
    BatchUniform {
        emitter: batch.emitter.to_cols_array_2d(),
        velocity: [batch.velocity.x, batch.velocity.y, batch.velocity.z, batch.interval],
        extent: [extent.x, extent.y, extent.z, age0],
        ids: [batch.first, batch.count, batch.seed, batch.shape.code() | colour_count << 4 | cell_count << 8],
        misc: [slot, batch.scale.to_bits(), 0, 0],
        cells,
        colours: packed_colours,
    }
}

/// One block of the frame: where it starts in the buffer, its system's slot and how many particle slots it draws.
#[derive(Copy, Clone)]
struct BlockDraw {
    offset: u32,
    system: u32,
    slots: u32,
}

/// The frame's blocks, packed on the CPU and kept between frames.
struct Packer {
    staging: Vec<u8>,
    /// Batch indices, grouped by system.
    order: Vec<u32>,
    blocks: Vec<BlockDraw>,
    align: usize,
}

impl Packer {
    fn new(align: usize) -> Self {
        Self {
            staging: vec![0; BUFFER_BYTES],
            order: Vec::with_capacity(limits::MAX_SPAWN_BATCHES),
            blocks: Vec::with_capacity(limits::MAX_SPAWN_BATCHES),
            align: align.clamp(1, 256),
        }
    }

    /// Packs `scene`'s batches (every one names a live system: `FrameState::prepare` checked) and returns the bytes
    /// used.
    fn pack(&mut self, scene: &Scene, systems: &Arena<GpuParticleSystem>, density: f32) -> usize {
        let batches = &scene.particles;
        self.order.clear();
        self.blocks.clear();
        // At most `MAX_SPAWN_BATCHES` in a valid scene.
        #[allow(clippy::cast_possible_truncation)]
        self.order.extend(0..batches.len() as u32);
        self.order.sort_unstable_by_key(|&i| (batches[i as usize].system.index(), i));
        let mut used = 0;
        let mut next = 0;
        while next < self.order.len() {
            let slot = batches[self.order[next] as usize].system.index();
            let Some(system) = systems.at(slot) else {
                next += 1;
                continue;
            };
            let offset = used;
            let (mut records, mut slots) = (0, 0u32);
            while next < self.order.len() && records < BLOCK_BATCHES {
                let batch = &batches[self.order[next] as usize];
                if batch.system.index() != slot {
                    break;
                }
                next += 1;
                if batch.count == 0 {
                    continue;
                }
                let at = offset + HEADER_BYTES + records * BATCH_BYTES;
                let record = batch_uniform(batch, system, scene.frame.time, slots);
                self.staging[at..at + BATCH_BYTES].copy_from_slice(bytemuck::bytes_of(&record));
                slots = slots.saturating_add(batch.count);
                records += 1;
            }
            if records == 0 {
                continue;
            }
            let mut header = system.header;
            header.edge[2] = density;
            // At most `BLOCK_BATCHES`.
            #[allow(clippy::cast_possible_truncation)]
            {
                header.atlas[3] = records as u32;
            }
            self.staging[offset..offset + HEADER_BYTES].copy_from_slice(bytemuck::bytes_of(&header));
            // Offsets stay far below 4 GiB (`BUFFER_BYTES`).
            #[allow(clippy::cast_possible_truncation)]
            self.blocks.push(BlockDraw { offset: offset as u32, system: slot, slots });
            used = (offset + HEADER_BYTES + records * BATCH_BYTES).next_multiple_of(self.align);
        }
        used
    }
}

/// The pass's pipelines at one sample count.
struct Pipelines {
    colour: wgpu::RenderPipeline,
    alpha: wgpu::RenderPipeline,
}

/// See the module docs.
pub(crate) struct Particles {
    module: wgpu::ShaderModule,
    group_layout: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    pipelines: HashMap<u32, Pipelines>,
    blocks: wgpu::Buffer,
    /// Group 0 over the frame uniforms and the blocks, built on the first frame (the frame uniforms live as long as
    /// the renderer).
    group: Option<wgpu::BindGroup>,
    packer: Packer,
}

/// The shader: the Opaque pass's module with no program, the evaluation, then the pass's own stages.
pub(crate) fn source() -> String {
    format!(
        "{}\n{}\n{}",
        opaque_source(None),
        include_str!("../shader/particles_state.wgsl"),
        include_str!("../shader/particles.wgsl")
    )
}

fn dynamic_uniform(binding: u32, size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: wgpu::BufferSize::new(size),
        },
        count: None,
    }
}

impl Particles {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        let group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particles"),
            entries: &[dynamic_uniform(0, FrameUniforms::SIZE), dynamic_uniform(3, BLOCK_BYTES as u64)],
        });
        let align = device.limits().min_uniform_buffer_offset_alignment as usize;
        Self {
            module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("particles"),
                source: wgpu::ShaderSource::Wgsl(source().into()),
            }),
            layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("particles"),
                bind_group_layouts: &[
                    Some(&group_layout),
                    Some(&shared.layouts.material),
                    Some(&shared.layouts.shadow_receiver),
                    Some(&shared.layouts.environment),
                ],
                immediate_size: 0,
            }),
            group_layout,
            pipelines: HashMap::new(),
            blocks: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("particle blocks"),
                size: BUFFER_BYTES as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            group: None,
            packer: Packer::new(align),
        }
    }
}

fn build_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    samples: u32,
    alpha: bool,
) -> wgpu::RenderPipeline {
    let (entry, compare, write, mask) = match (alpha, samples > 1) {
        (true, _) => ("fs_particle_alpha", wgpu::CompareFunction::Equal, false, wgpu::ColorWrites::ALPHA),
        (false, true) => ("fs_particle", wgpu::CompareFunction::LessEqual, true, wgpu::ColorWrites::COLOR),
        (false, false) => ("fs_particle_tested", wgpu::CompareFunction::LessEqual, true, wgpu::ColorWrites::COLOR),
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if alpha { "particles alpha" } else { "particles colour" }),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_particle"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(compare),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: samples,
            alpha_to_coverage_enabled: !alpha && samples > 1,
            ..Default::default()
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState { format: SCENE_FORMAT, blend: None, write_mask: mask })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl FramePass for Particles {
    fn pass(&self) -> Pass {
        Pass::Particles
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        let (frame, resources) = (cx.frame, cx.resources);
        if frame.draws.particle_slots == 0 {
            return Ok(Outcome::Skipped);
        }
        let device = &cx.shared.gpu.device;
        // The blocks are the same in every round; only the frame uniform's offset moves.
        if cx.round == 0 {
            let used = self.packer.pack(frame.scene, resources.particles, frame.quality.particle_density);
            if used > 0 {
                cx.shared.gpu.queue.write_buffer(&self.blocks, 0, &self.packer.staging[..used]);
            }
        }
        if self.packer.blocks.is_empty() {
            return Ok(Outcome::Skipped);
        }
        let samples = frame.samples;
        let pipelines = self.pipelines.entry(samples).or_insert_with(|| Pipelines {
            colour: build_pipeline(device, &self.module, &self.layout, samples, false),
            alpha: build_pipeline(device, &self.module, &self.layout, samples, true),
        });
        let group = self.group.get_or_insert_with(|| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("particles"),
                layout: &self.group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &frame.uniforms.buffer,
                            offset: 0,
                            size: wgpu::BufferSize::new(FrameUniforms::SIZE),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.blocks,
                            offset: 0,
                            size: wgpu::BufferSize::new(BLOCK_BYTES as u64),
                        }),
                    },
                ],
            })
        });
        // At most `MAX_ROUNDS` rounds of one aligned uniform each.
        #[allow(clippy::cast_possible_truncation)]
        let round_offset = (cx.round as u64 * frame.uniforms.stride) as u32;

        let environment = cx.environment_group();
        let mut pass = cx.attachments.geometry_pass(cx.encoder, "particles", frame.clear_colour());
        pass.set_bind_group(2, cx.attachments.shadow.receiver_group(), &[]);
        pass.set_bind_group(3, environment, &[]);
        for block in &self.packer.blocks {
            let Some(system) = resources.particles.at(block.system) else { continue };
            pass.set_bind_group(0, &*group, &[round_offset, block.offset]);
            pass.set_bind_group(1, &system.material.bind_group, &[]);
            pass.set_pipeline(&pipelines.colour);
            pass.draw(0..4, 0..block.slots);
            pass.set_pipeline(&pipelines.alpha);
            pass.draw(0..4, 0..block.slots);
            cx.stats.draw_calls += 2;
            cx.stats.triangles += 4 * u64::from(block.slots);
        }
        Ok(Outcome::Ran)
    }
}
