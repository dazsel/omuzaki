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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod tests {
    use std::f32::consts::PI;
    use std::sync::mpsc;

    use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3};
    use omuzaki_render_api::particles::evaluate;
    use omuzaki_render_api::{
        CellPick, Colour, ColourSet, EmitterShape, Flutter, Orientation, ParticleParams, ParticleShading,
        ParticleSystemDesc, ParticleSystemId, Span, Sprite, SpriteShape, TextureId,
    };

    use super::*;
    use crate::gpu::{AdapterPreference, Gpu, HeadlessOptions};

    fn params(shape_orientation: Orientation) -> ParticleParams {
        ParticleParams {
            lifetime: Span::new(2.0, 5.0),
            speed: Span::new(0.5, 2.5),
            direction: [0.2, 1.0, -0.1],
            spread_deg: 35.0,
            gravity: [0.0, -2.0, 0.0],
            drag: 0.0,
            wind: [0.0; 3],
            flutter: None,
            size: Span::new(0.02, 0.06),
            size_curve: [0.3, 1.0, 0.8, 0.0],
            orientation: shape_orientation,
            colours: ColourSet::new(&[Colour::rgb(250, 90, 120), Colour::rgb(80, 200, 255), Colour::rgb(255, 230, 90)])
                .unwrap(),
            coverage: 1.0,
            shading: ParticleShading::Unlit,
            edge: None,
            glow: 0.0,
            floor: None,
        }
    }

    /// One system per shape and orientation, with drag, wind, flutter, a floor, and an atlas with listed and any cells.
    fn systems() -> Vec<ParticleSystemDesc> {
        let shape = |s| Sprite::Shape(s);
        let mut confetti = params(Orientation::tumble(Span::new(-400.0, 400.0)));
        (confetti.drag, confetti.spread_deg, confetti.wind) = (0.8, 180.0, [0.3, 0.0, -0.2]);
        confetti.flutter = Some(Flutter { amplitude_m: 0.1, frequency_hz: 1.3 });
        let mut snow = params(Orientation::billboard(Span::new(-90.0, 90.0)));
        (snow.drag, snow.floor) = (1.5, Some(-0.5));
        snow.flutter = Some(Flutter { amplitude_m: 0.2, frequency_hz: 0.4 });
        let mut rain = params(Orientation::velocity(0.05));
        (rain.gravity, rain.floor, rain.spread_deg) = ([0.0, -9.8, 0.0], Some(-1.0), 0.0);
        let mut stars = params(Orientation::billboard(Span::fixed(0.0)));
        stars.spread_deg = 180.0;
        let atlas = Sprite::atlas(TextureId::from_parts(0, 1), 4, 3);
        vec![
            ParticleSystemDesc { sprite: shape(SpriteShape::rect(0.5)), params: confetti },
            ParticleSystemDesc { sprite: shape(SpriteShape::Disc), params: snow },
            ParticleSystemDesc { sprite: shape(SpriteShape::Streak), params: rain },
            ParticleSystemDesc { sprite: shape(SpriteShape::star(5)), params: stars.clone() },
            ParticleSystemDesc {
                sprite: shape(SpriteShape::Heart),
                params: params(Orientation::tumble(Span::fixed(90.0))),
            },
            ParticleSystemDesc { sprite: atlas, params: stars },
        ]
    }

    /// `count` batches of `system`: bursts and streams from each emitter shape, turned and moving.
    fn batches(system: ParticleSystemId, count: u32, per_batch: u32) -> Vec<SpawnBatch> {
        let shapes = [
            EmitterShape::Point,
            EmitterShape::sphere(0.4),
            EmitterShape::cuboid(Vec3::new(0.5, 0.2, 0.3)),
            EmitterShape::rect(Vec2::new(1.5, 0.8)),
        ];
        (0..count)
            .map(|i| {
                let f = i as f32;
                let turn = Quat::from_euler(omuzaki_core::glam::EulerRot::YXZ, 0.3 * f, 0.2 * f - 0.4, 0.1 * f);
                let emitter = Mat4::from_rotation_translation(turn, Vec3::new(f * 0.1 - 1.0, 0.5, -f * 0.05));
                SpawnBatch {
                    shape: shapes[i as usize % shapes.len()],
                    velocity: Vec3::new(0.1 * f, 0.0, -0.05),
                    start: Seconds(0.1 * f64::from(i % 7)),
                    interval: if i % 3 == 0 { 0.0 } else { 0.01 * (1 + i % 5) as f32 },
                    first: 37 * i,
                    count: per_batch,
                    seed: 0x5EED_0000 ^ i.wrapping_mul(2_654_435_761),
                    cells: if i % 2 == 0 { CellPick::All } else { CellPick::list(&[1, 5, 11]).unwrap() },
                    colours: (i % 4 == 1).then(|| ColourSet::one(Colour::rgb(10, 250, 40))),
                    scale: 0.5 + 0.1 * (i % 6) as f32,
                    ..SpawnBatch::new(system, emitter)
                }
            })
            .collect()
    }

    /// Runs `particles_state.wgsl` on every slot of the block at offset 0 and returns three vectors per slot:
    /// (position, alive), (velocity, roll), (extent, cell, colour red).
    fn probe(gpu: &Gpu, blocks: &[u8], slots: u32) -> Vec<[f32; 4]> {
        let device = &gpu.device;
        let source = format!(
            "{}\n{}\n{}",
            include_str!("../shader/common.wgsl"),
            include_str!("../shader/particles_state.wgsl"),
            r"
@group(0) @binding(4) var<storage, read_write> probes: array<vec4<f32>>;

@compute @workgroup_size(64)
fn cs_probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = id.x;
    if (slot * 3u + 2u >= arrayLength(&probes)) {
        return;
    }
    let r = find_batch(slot);
    let s = particle_state(r, block.batches[r].ids.x + (slot - block.batches[r].misc.x));
    probes[slot * 3u] = vec4<f32>(s.position, select(0.0, 1.0, s.alive));
    probes[slot * 3u + 1u] = vec4<f32>(s.velocity, s.roll);
    probes[slot * 3u + 2u] = vec4<f32>(s.extent, f32(s.cell), s.colour.r);
}
"
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particle parity"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("particle parity"),
            layout: None,
            module: &module,
            entry_point: Some("cs_probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: BLOCK_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&uniform, 0, &blocks[..blocks.len().min(BLOCK_BYTES)]);
        let bytes = u64::from(slots) * 48;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 3, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: output.as_entire_binding() },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(slots.div_ceil(64), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &read, 0, bytes);
        gpu.queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        read.slice(..).map_async(wgpu::MapMode::Read, move |r| sender.send(r).unwrap());
        gpu.wait_idle(crate::gpu::WAIT_LIMIT).unwrap();
        receiver.recv().unwrap().unwrap();
        bytemuck::cast_slice::<u8, [f32; 4]>(&read.slice(..).get_mapped_range().unwrap()).to_vec()
    }

    /// D6's parity test: 10,800 particles over every shape, orientation and emitter, evaluated by the shader, match
    /// `particles::evaluate` within 1e-4 m plus 1e-5 of the distance from the origin.
    #[test]
    fn the_shader_evaluates_particles_as_the_reference_does() {
        let gpu = Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap();
        let shared = Shared::new(gpu);
        let mut arena = Arena::default();
        let mut packer = Packer::new(256);
        let (mut compared, mut alive, mut flipped, mut worst) = (0, 0, 0, 0.0f32);
        for (k, desc) in systems().into_iter().enumerate() {
            desc.validate().unwrap();
            let (index, generation) = arena.insert(GpuParticleSystem::create(&shared, &Arena::default(), &desc));
            let id = ParticleSystemId::from_parts(index, generation);
            let mut scene = Scene::new(omuzaki_render_api::Camera::default());
            scene.particles = batches(id, 90, 20);
            scene.frame.time = Seconds(1.1 + 0.2 * k as f64);
            let density = if k == 3 { 0.5 } else { 1.0 };
            let used = packer.pack(&scene, &arena, density);
            assert_eq!(packer.blocks.len(), 1, "90 batches of one system are one block");
            let slots = packer.blocks[0].slots;
            let gpu_values = probe(&shared.gpu, &packer.staging[..used], slots);
            let mut slot = 0usize;
            for batch in &scene.particles {
                for g in batch.first..batch.first + batch.count {
                    let [position, velocity, extent] =
                        [gpu_values[slot * 3], gpu_values[slot * 3 + 1], gpu_values[slot * 3 + 2]];
                    slot += 1;
                    compared += 1;
                    let reference = evaluate(&desc, batch, g, scene.frame.time, density);
                    let Some(state) = reference else {
                        flipped += usize::from(position[3] > 0.5);
                        continue;
                    };
                    if position[3] < 0.5 {
                        flipped += 1;
                        continue;
                    }
                    alive += 1;
                    let got = Vec3::new(position[0], position[1], position[2]);
                    let error = got.distance(state.position);
                    let allowed = 1e-4 + 1e-5 * state.position.length();
                    worst = worst.max(error / allowed);
                    assert!(error <= allowed, "system {k} particle {g}: {got} against {}", state.position);
                    let v = Vec3::new(velocity[0], velocity[1], velocity[2]);
                    assert!(v.distance(state.velocity) <= 1e-4 * (1.0 + state.velocity.length()), "velocity");
                    let e = Vec2::new(extent[0], extent[1]);
                    assert!((e - state.extent).abs().max_element() <= 1e-5 + 1e-4 * state.extent.max_element());
                    assert_eq!(extent[2] as u32, state.cell, "system {k} particle {g}: the cell");
                    assert!((extent[3] - state.colour.x).abs() <= 1e-4, "the colour");
                    let roll_error = (velocity[3] - state.roll).abs() % (2.0 * PI);
                    assert!(roll_error.min(2.0 * PI - roll_error) <= 1e-3, "system {k} particle {g}: the roll");
                }
            }
        }
        eprintln!(
            "compared {compared} particles, {alive} alive, {flipped} on a boundary, worst {worst:.3} of the allowance"
        );
        assert!(compared >= 10_000 && alive > compared / 3, "{alive} of {compared} alive");
        // Life, the density test and the floor are comparisons of exactly computed numbers; a contracted multiply-add
        // can move one at a boundary, never more than a handful.
        assert!(flipped <= compared / 1000, "{flipped} particles alive on one side only");
    }

    #[test]
    fn a_block_is_a_header_and_its_records_at_aligned_offsets() {
        assert_eq!(BLOCK_BYTES, 16_272);
        let source = include_str!("../shader/particles_state.wgsl");
        assert!(source.contains(&format!("const BLOCK_BATCHES: u32 = {BLOCK_BATCHES}u;")));
    }

    /// #756 (particles.md D6): the pass is one shader for the three capability profiles. Its entry points read no
    /// storage buffer and there is no compute stage; what they do read fits WebGL2's limits, so the particles add
    /// nothing to the list of what WebGL2 lacks (that list is the renderer's instance and joint storage buffers).
    #[test]
    fn the_particle_stages_read_no_storage_and_fit_webgl2() {
        use wgpu::naga::{self, AddressSpace, ShaderStage, TypeInner};

        let module = naga::front::wgsl::parse_str(&source()).unwrap();
        let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap();
        assert!(module.entry_points.iter().all(|e| e.stage != ShaderStage::Compute), "the module has no compute stage");
        let webgl2 = crate::caps::CapabilityProfile::WebGl2.limits();
        let mut seen = Vec::new();
        for (index, entry) in module.entry_points.iter().enumerate() {
            if !entry.name.contains("particle") {
                continue;
            }
            seen.push(entry.name.as_str());
            let used = info.get_entry_point(index);
            let (mut uniforms, mut textures, mut samplers) = (0, 0, 0);
            for (handle, global) in module.global_variables.iter() {
                if used[handle].is_empty() {
                    continue;
                }
                let inner = &module.types[global.ty].inner;
                match global.space {
                    AddressSpace::Storage { .. } => {
                        panic!("{}: reads the storage buffer {:?}", entry.name, global.name)
                    }
                    AddressSpace::Uniform => {
                        uniforms += 1;
                        let size = inner.size(module.to_ctx());
                        assert!(
                            size <= webgl2.max_uniform_buffer_binding_size as u32,
                            "{}: {:?} is {size} bytes",
                            entry.name,
                            global.name
                        );
                    }
                    AddressSpace::Handle => match inner {
                        TypeInner::Sampler { .. } => samplers += 1,
                        TypeInner::Image { .. } => textures += 1,
                        _ => {}
                    },
                    _ => {}
                }
            }
            assert!(uniforms <= webgl2.max_uniform_buffers_per_shader_stage, "{}: {uniforms} uniforms", entry.name);
            assert!(textures <= webgl2.max_sampled_textures_per_shader_stage, "{}: {textures} textures", entry.name);
            assert!(samplers <= webgl2.max_samplers_per_shader_stage, "{}: {samplers} samplers", entry.name);
        }
        seen.sort_unstable();
        assert_eq!(seen, ["fs_particle", "fs_particle_alpha", "fs_particle_tested", "vs_particle"]);
    }
}
