//! Vello's compute pipeline, run with resources that persist between repaints (#2074).
//!
//! `vello::Renderer::render_to_texture` rebuilds its whole GPU side on every call: a new recording, about 20 bind
//! groups, a buffer-pool lookup per buffer, a fresh gradient texture whenever the scene has a gradient, and its own
//! encoder and submit. On the development PC that cost 0.77 ms of CPU and 816 allocations per face repaint, against
//! 0.2 ms of GPU work (the profile on #2074). This module runs the same shaders (`vello_shaders`, the stages
//! `vello::low_level::Render` records) on buffers, textures and bind groups made once and grown only when a scene needs
//! more room, and records into the caller's encoder: a repaint writes the packed scene and the config into buffers it
//! owns and records one compute pass of the stages, and allocates nothing of its own once warm.
//!
//! The stage list and the bindings of each are Vello 0.11's `render_encoding_coarse` and `record_fine` with area
//! anti-aliasing, the mode the face uses; the version is pinned, so they cannot drift apart unnoticed (the face GPU
//! tests compare every probe against the CPU raster).

use vello::Scene;
use vello::peniko::Color;
use vello_encoding::{BufferSizes, ConfigUniform, RenderConfig, Resolver, WorkgroupSize};
use vello_shaders::{BindType, ComputeShader};

/// The Vello stages this engine dispatches, in the order they run.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Stage {
    PathtagReduce,
    PathtagReduce2,
    PathtagScan1,
    PathtagScanSmall,
    PathtagScanLarge,
    BboxClear,
    Flatten,
    DrawReduce,
    DrawLeaf,
    ClipReduce,
    ClipLeaf,
    Binning,
    TileAlloc,
    PathCountSetup,
    PathCount,
    Backdrop,
    Coarse,
    PathTilingSetup,
    PathTiling,
    Fine,
}

/// How many stages there are.
const STAGES: usize = 20;

impl Stage {
    const ALL: [Self; STAGES] = [
        Self::PathtagReduce,
        Self::PathtagReduce2,
        Self::PathtagScan1,
        Self::PathtagScanSmall,
        Self::PathtagScanLarge,
        Self::BboxClear,
        Self::Flatten,
        Self::DrawReduce,
        Self::DrawLeaf,
        Self::ClipReduce,
        Self::ClipLeaf,
        Self::Binning,
        Self::TileAlloc,
        Self::PathCountSetup,
        Self::PathCount,
        Self::Backdrop,
        Self::Coarse,
        Self::PathTilingSetup,
        Self::PathTiling,
        Self::Fine,
    ];

    /// The shader of this stage in Vello's generated table.
    fn shader(self) -> &'static ComputeShader<'static> {
        let s = &vello_shaders::SHADERS;
        match self {
            Self::PathtagReduce => &s.pathtag_reduce,
            Self::PathtagReduce2 => &s.pathtag_reduce2,
            Self::PathtagScan1 => &s.pathtag_scan1,
            Self::PathtagScanSmall => &s.pathtag_scan_small,
            Self::PathtagScanLarge => &s.pathtag_scan_large,
            Self::BboxClear => &s.bbox_clear,
            Self::Flatten => &s.flatten,
            Self::DrawReduce => &s.draw_reduce,
            Self::DrawLeaf => &s.draw_leaf,
            Self::ClipReduce => &s.clip_reduce,
            Self::ClipLeaf => &s.clip_leaf,
            Self::Binning => &s.binning,
            Self::TileAlloc => &s.tile_alloc,
            Self::PathCountSetup => &s.path_count_setup,
            Self::PathCount => &s.path_count,
            Self::Backdrop => &s.backdrop_dyn,
            Self::Coarse => &s.coarse,
            Self::PathTilingSetup => &s.path_tiling_setup,
            Self::PathTiling => &s.path_tiling,
            Self::Fine => &s.fine_area,
        }
    }

    /// What this stage binds, in the order of its shader's bindings (Vello's `render_encoding_coarse` and
    /// `record_fine`). The large path-tag scan reads `ReducedScan` where the small one reads `Reduced`.
    fn bindings(self) -> &'static [Res] {
        use Res::{
            Atlas, BinHeader, BlendSpill, Bump, ClipBbox, ClipBic, ClipEl, ClipInp, Config, DrawBbox, DrawMonoid,
            DrawReduced, Gradients, IndirectCount, InfoBinData, Lines, Out, Path, PathBbox, Ptcl, Reduced, Reduced2,
            ReducedScan, Scene, SegCounts, Segments, TagMonoid, Tile,
        };
        match self {
            Self::PathtagReduce => &[Config, Scene, Reduced],
            Self::PathtagReduce2 => &[Reduced, Reduced2],
            Self::PathtagScan1 => &[Reduced, Reduced2, ReducedScan],
            Self::PathtagScanSmall => &[Config, Scene, Reduced, TagMonoid],
            Self::PathtagScanLarge => &[Config, Scene, ReducedScan, TagMonoid],
            Self::BboxClear => &[Config, PathBbox],
            Self::Flatten => &[Config, Scene, TagMonoid, PathBbox, Bump, Lines],
            Self::DrawReduce => &[Config, Scene, DrawReduced],
            Self::DrawLeaf => &[Config, Scene, DrawReduced, PathBbox, DrawMonoid, InfoBinData, ClipInp],
            Self::ClipReduce => &[ClipInp, PathBbox, ClipBic, ClipEl],
            Self::ClipLeaf => &[Config, ClipInp, PathBbox, ClipBic, ClipEl, DrawMonoid, ClipBbox],
            Self::Binning => &[Config, DrawMonoid, PathBbox, ClipBbox, DrawBbox, Bump, InfoBinData, BinHeader],
            Self::TileAlloc => &[Config, Scene, DrawBbox, Bump, Path, Tile],
            Self::PathCountSetup => &[Bump, IndirectCount],
            Self::PathCount => &[Config, Bump, Lines, Path, Tile, SegCounts],
            Self::Backdrop => &[Config, Bump, Path, Tile],
            Self::Coarse => &[Config, Scene, DrawMonoid, BinHeader, InfoBinData, Path, Tile, Bump, Ptcl],
            Self::PathTilingSetup => &[Bump, IndirectCount, Ptcl],
            Self::PathTiling => &[Bump, SegCounts, Lines, Path, Tile, Segments],
            Self::Fine => &[Config, Segments, Ptcl, InfoBinData, BlendSpill, Out, Gradients, Atlas],
        }
    }
}

/// A resource a stage binds. Everything up to `Ptcl` is a buffer shared by every layer (the stages of one repaint
/// finish before the next repaint's start, so they can share scratch); `Config`, `Scene`, `Out` and `Gradients` are the
/// layer's own, because a layer's uploads must survive until its commands run.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Res {
    Reduced,
    Reduced2,
    ReducedScan,
    TagMonoid,
    PathBbox,
    Bump,
    Lines,
    DrawReduced,
    DrawMonoid,
    InfoBinData,
    ClipInp,
    ClipEl,
    ClipBic,
    ClipBbox,
    DrawBbox,
    BinHeader,
    Path,
    Tile,
    IndirectCount,
    SegCounts,
    Segments,
    BlendSpill,
    Ptcl,
    Config,
    Scene,
    Out,
    Gradients,
    Atlas,
}

/// How many of [`Res`] are shared buffers (`Reduced` through `Ptcl`).
const SHARED: usize = Res::Ptcl as usize + 1;

impl Res {
    /// The bytes a shared buffer needs for a scene of these sizes; `None` for the layer's own resources.
    fn size(self, sizes: &BufferSizes) -> Option<u32> {
        let bytes = match self {
            Self::Reduced => sizes.path_reduced.size_in_bytes(),
            Self::Reduced2 => sizes.path_reduced2.size_in_bytes(),
            Self::ReducedScan => sizes.path_reduced_scan.size_in_bytes(),
            Self::TagMonoid => sizes.path_monoids.size_in_bytes(),
            Self::PathBbox => sizes.path_bboxes.size_in_bytes(),
            Self::Bump => sizes.bump_alloc.size_in_bytes(),
            Self::Lines => sizes.lines.size_in_bytes(),
            Self::DrawReduced => sizes.draw_reduced.size_in_bytes(),
            Self::DrawMonoid => sizes.draw_monoids.size_in_bytes(),
            Self::InfoBinData => sizes.bin_data.size_in_bytes(),
            Self::ClipInp => sizes.clip_inps.size_in_bytes(),
            Self::ClipEl => sizes.clip_els.size_in_bytes(),
            Self::ClipBic => sizes.clip_bics.size_in_bytes(),
            Self::ClipBbox => sizes.clip_bboxes.size_in_bytes(),
            Self::DrawBbox => sizes.draw_bboxes.size_in_bytes(),
            Self::BinHeader => sizes.bin_headers.size_in_bytes(),
            Self::Path => sizes.paths.size_in_bytes(),
            Self::Tile => sizes.tiles.size_in_bytes(),
            Self::IndirectCount => sizes.indirect_count.size_in_bytes(),
            Self::SegCounts => sizes.seg_counts.size_in_bytes(),
            Self::Segments => sizes.segments.size_in_bytes(),
            Self::BlendSpill => sizes.blend_spill.size_in_bytes(),
            Self::Ptcl => sizes.ptcl.size_in_bytes(),
            Self::Config | Self::Scene | Self::Out | Self::Gradients | Self::Atlas => return None,
        };
        Some(bytes)
    }

    /// The shared buffers, in [`Res`] order.
    const SHARED: [Self; SHARED] = [
        Self::Reduced,
        Self::Reduced2,
        Self::ReducedScan,
        Self::TagMonoid,
        Self::PathBbox,
        Self::Bump,
        Self::Lines,
        Self::DrawReduced,
        Self::DrawMonoid,
        Self::InfoBinData,
        Self::ClipInp,
        Self::ClipEl,
        Self::ClipBic,
        Self::ClipBbox,
        Self::DrawBbox,
        Self::BinHeader,
        Self::Path,
        Self::Tile,
        Self::IndirectCount,
        Self::SegCounts,
        Self::Segments,
        Self::BlendSpill,
        Self::Ptcl,
    ];
}

/// The width of Vello's gradient ramps, in texels (`vello_encoding`'s `N_SAMPLES`).
const RAMP_WIDTH: u32 = 512;
/// The smallest buffer made: wgpu refuses empty bindings, and Vello's shaders read at least one element of each.
const MIN_BUFFER: u64 = 256;

/// `bytes` rounded up so a buffer that grows does so a handful of times, not on every slightly larger scene.
fn capacity_for(bytes: u64) -> u64 {
    bytes.max(MIN_BUFFER).next_power_of_two()
}

/// One stage compiled: its pipeline and the layout its bind groups follow.
struct Compiled {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

/// The bind group layout entries of `shader`, as Vello's own engine makes them.
fn layout_entries(shader: &'static ComputeShader<'static>) -> impl Iterator<Item = wgpu::BindGroupLayoutEntry> {
    shader.bindings.iter().zip(shader.wgsl.binding_indices.iter()).map(|(&kind, &binding)| {
        let ty = match kind {
            BindType::Buffer | BindType::BufReadOnly => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: kind == BindType::BufReadOnly },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            BindType::Uniform => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            BindType::Image => wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::Rgba8Unorm,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            BindType::ImageRead => wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
        };
        wgpu::BindGroupLayoutEntry {
            binding: u32::from(binding),
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        }
    })
}

impl Compiled {
    fn new(device: &wgpu::Device, stage: Stage) -> Self {
        let shader = stage.shader();
        let entries: Vec<_> = layout_entries(shader).collect();
        let layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &entries });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&shader.name),
            source: wgpu::ShaderSource::Wgsl(shader.wgsl.code.clone()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&shader.name),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: None,
            // As Vello's own engine: every stage initialises the workgroup memory it reads.
            compilation_options: wgpu::PipelineCompilationOptions {
                zero_initialize_workgroup_memory: false,
                ..wgpu::PipelineCompilationOptions::default()
            },
            cache: None,
        });
        Self { pipeline, layout }
    }
}

/// The scratch buffers every layer's repaint shares, with how many times they have been remade (layers rebuild their
/// bind groups when it moves).
struct Scratch {
    buffers: [wgpu::Buffer; SHARED],
    generation: u64,
}

impl Scratch {
    fn buffer(device: &wgpu::Device, res: Res, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("face-vello-scratch"),
            size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | if res == Res::IndirectCount { wgpu::BufferUsages::INDIRECT } else { wgpu::BufferUsages::empty() },
            mapped_at_creation: false,
        })
    }

    fn new(device: &wgpu::Device, sizes: &BufferSizes) -> Self {
        let buffers = Res::SHARED
            .map(|res| Self::buffer(device, res, capacity_for(u64::from(res.size(sizes).unwrap_or_default()))));
        Self { buffers, generation: 0 }
    }

    /// Makes every buffer at least as large as `sizes` asks; true if any was remade.
    fn fit(&mut self, device: &wgpu::Device, sizes: &BufferSizes) -> bool {
        let mut grew = false;
        for (slot, res) in self.buffers.iter_mut().zip(Res::SHARED) {
            let needed = u64::from(res.size(sizes).unwrap_or_default());
            if slot.size() < needed {
                *slot = Self::buffer(device, res, capacity_for(needed));
                grew = true;
            }
        }
        if grew {
            self.generation += 1;
        }
        grew
    }
}

/// What one layer owns: its uploads, the texture the fine stage writes and the bind groups that name them.
pub(super) struct LayerTarget {
    scene: wgpu::Buffer,
    config: wgpu::Buffer,
    gradients: wgpu::Texture,
    gradients_view: wgpu::TextureView,
    /// The ramps last written into `gradients`, so an unchanged set is not uploaded again.
    ramps: Vec<u32>,
    /// The bind groups of every stage, and the scratch generation they were made for. `None` after anything they name
    /// was remade.
    groups: Option<(u64, [wgpu::BindGroup; STAGES])>,
}

impl LayerTarget {
    /// A layer's resources, sized for nothing yet.
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let (gradients, gradients_view) = gradient_texture(device, 1);
        Self {
            scene: scene_buffer(device, MIN_BUFFER),
            config: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("face-vello-config"),
                size: std::mem::size_of::<ConfigUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            gradients,
            gradients_view,
            ramps: Vec::new(),
            groups: None,
        }
    }

    /// Forgets the bind groups: the texture the fine stage writes was remade.
    pub(super) fn out_changed(&mut self) {
        self.groups = None;
    }
}

fn scene_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("face-vello-scene"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn gradient_texture(device: &wgpu::Device, rows: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("face-vello-gradients"),
        size: wgpu::Extent3d { width: RAMP_WIDTH, height: rows.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// What [`Engine::record`] needs to know about the picture it rasterises.
pub(super) struct Frame<'a> {
    /// The scene, already shifted so the rectangle to draw starts at (0, 0).
    pub(super) scene: &'a Scene,
    /// The rectangle's size in pixels.
    pub(super) width: u32,
    pub(super) height: u32,
    /// The texture the fine stage writes: straight-alpha `Rgba8Unorm` with storage binding, at least `width` x `height`.
    pub(super) out: &'a wgpu::TextureView,
    /// Where the compute pass writes its begin and end timestamps (queries `at` and `at + 1`), for the timing probe.
    pub(super) timestamps: Option<(&'a wgpu::QuerySet, u32)>,
}

/// Vello's pipeline on one device: the compiled stages, the resolver that packs a scene, the shared scratch buffers and
/// the image atlas the fine stage binds (the face draws no images, so it is one empty texel).
pub(super) struct Engine {
    stages: [Compiled; STAGES],
    resolver: Resolver,
    /// The packed scene of the last repaint, kept so packing the next one allocates nothing.
    packed: Vec<u8>,
    scratch: Option<Scratch>,
    atlas: wgpu::TextureView,
}

impl Engine {
    /// Compiles every stage for `device`. Slow (pipeline compilation): one per rasteriser, on the first paint.
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let atlas = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("face-vello-atlas"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            stages: Stage::ALL.map(|stage| Compiled::new(device, stage)),
            resolver: Resolver::new(),
            packed: Vec::new(),
            scratch: None,
            atlas,
        }
    }

    /// Records the rasterisation of `frame` into `encoder`, writing the frame's scene and config into `target`'s buffers
    /// through `queue` (ordered before `encoder`'s commands once it is submitted).
    ///
    /// # Errors
    /// A message if the scene draws images or glyphs (the face draws neither, and the image atlas is a stand-in).
    pub(super) fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &mut LayerTarget,
        frame: &Frame<'_>,
    ) -> Result<(), String> {
        let (layout, ramps, images) = self.resolver.resolve(frame.scene.encoding(), &mut self.packed);
        if !images.images.is_empty() {
            return Err("the scene draws images, which the face rasteriser does not support".to_owned());
        }
        let config = RenderConfig::new(&layout, frame.width, frame.height, &Color::TRANSPARENT);
        if self.packed.is_empty() {
            // wgpu refuses an empty binding; the config says the scene is empty, so this word is never read (as Vello).
            self.packed.resize(std::mem::size_of::<u32>(), u8::MAX);
        }

        let scratch = match &mut self.scratch {
            Some(scratch) => {
                scratch.fit(device, &config.buffer_sizes);
                scratch
            }
            empty @ None => empty.insert(Scratch::new(device, &config.buffer_sizes)),
        };
        if target.scene.size() < self.packed.len() as u64 {
            target.scene = scene_buffer(device, capacity_for(self.packed.len() as u64));
            target.groups = None;
        }
        if ramps.height > 0 && target.ramps.as_slice() != ramps.data {
            if target.gradients.height() < ramps.height {
                (target.gradients, target.gradients_view) = gradient_texture(device, ramps.height.next_power_of_two());
                target.groups = None;
            }
            write_ramps(queue, &target.gradients, ramps.data, ramps.height);
            target.ramps.clear();
            target.ramps.extend_from_slice(ramps.data);
        }
        queue.write_buffer(&target.scene, 0, &self.packed);
        queue.write_buffer(&target.config, 0, bytemuck::bytes_of(&config.gpu));

        if target.groups.as_ref().is_none_or(|(generation, _)| *generation != scratch.generation) {
            let stages = &self.stages;
            let groups = Stage::ALL.map(|stage| {
                bind_group(device, stage, &stages[stage as usize], scratch, target, frame.out, &self.atlas)
            });
            target.groups = Some((scratch.generation, groups));
        }
        let Some((_, groups)) = target.groups.as_ref() else {
            return Err("the bind groups are missing after they were made".to_owned());
        };

        encoder.clear_buffer(&scratch.buffers[Res::Bump as usize], 0, None);
        let timestamp_writes = frame.timestamps.map(|(query_set, at)| wgpu::ComputePassTimestampWrites {
            query_set,
            beginning_of_pass_write_index: Some(at),
            end_of_pass_write_index: Some(at + 1),
        });
        let mut pass =
            encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("face-vello"), timestamp_writes });
        let wg = &config.workgroup_counts;
        let indirect = &scratch.buffers[Res::IndirectCount as usize];
        let stages = &self.stages;
        let mut run = |stage: Stage, size: Dispatch<'_>| {
            // `Stage::ALL` lists the stages by discriminant, so both arrays are indexed by it (a test checks).
            let (compiled, group) = (&stages[stage as usize], &groups[stage as usize]);
            match size {
                // Vello skips empty dispatches too (vello#637): some drivers mishandle them.
                Dispatch::Direct((x, y, z)) if x == 0 || y == 0 || z == 0 => {}
                Dispatch::Direct((x, y, z)) => {
                    pass.set_pipeline(&compiled.pipeline);
                    pass.set_bind_group(0, group, &[]);
                    pass.dispatch_workgroups(x, y, z);
                }
                Dispatch::Indirect(buffer) => {
                    pass.set_pipeline(&compiled.pipeline);
                    pass.set_bind_group(0, group, &[]);
                    pass.dispatch_workgroups_indirect(buffer, 0);
                }
            }
        };
        run(Stage::PathtagReduce, Dispatch::Direct(wg.path_reduce));
        if wg.use_large_path_scan {
            run(Stage::PathtagReduce2, Dispatch::Direct(wg.path_reduce2));
            run(Stage::PathtagScan1, Dispatch::Direct(wg.path_scan1));
            run(Stage::PathtagScanLarge, Dispatch::Direct(wg.path_scan));
        } else {
            run(Stage::PathtagScanSmall, Dispatch::Direct(wg.path_scan));
        }
        run(Stage::BboxClear, Dispatch::Direct(wg.bbox_clear));
        run(Stage::Flatten, Dispatch::Direct(wg.flatten));
        run(Stage::DrawReduce, Dispatch::Direct(wg.draw_reduce));
        run(Stage::DrawLeaf, Dispatch::Direct(wg.draw_leaf));
        run(Stage::ClipReduce, Dispatch::Direct(wg.clip_reduce));
        run(Stage::ClipLeaf, Dispatch::Direct(wg.clip_leaf));
        run(Stage::Binning, Dispatch::Direct(wg.binning));
        run(Stage::TileAlloc, Dispatch::Direct(wg.tile_alloc));
        run(Stage::PathCountSetup, Dispatch::Direct(wg.path_count_setup));
        run(Stage::PathCount, Dispatch::Indirect(indirect));
        run(Stage::Backdrop, Dispatch::Direct(wg.backdrop));
        run(Stage::Coarse, Dispatch::Direct(wg.coarse));
        run(Stage::PathTilingSetup, Dispatch::Direct(wg.path_tiling_setup));
        run(Stage::PathTiling, Dispatch::Indirect(indirect));
        run(Stage::Fine, Dispatch::Direct(wg.fine));
        Ok(())
    }
}

/// How a stage is dispatched: a fixed workgroup count, or the count an earlier stage wrote into a buffer.
enum Dispatch<'a> {
    Direct(WorkgroupSize),
    Indirect(&'a wgpu::Buffer),
}

/// Uploads the first `rows` ramps of `data` into `texture`.
fn write_ramps(queue: &wgpu::Queue, texture: &wgpu::Texture, data: &[u32], rows: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(data),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(RAMP_WIDTH * 4), rows_per_image: None },
        wgpu::Extent3d { width: RAMP_WIDTH, height: rows, depth_or_array_layers: 1 },
    );
}

/// The bind group of `stage` over the shared scratch and `target`'s own resources.
fn bind_group(
    device: &wgpu::Device,
    stage: Stage,
    compiled: &Compiled,
    scratch: &Scratch,
    target: &LayerTarget,
    out: &wgpu::TextureView,
    atlas: &wgpu::TextureView,
) -> wgpu::BindGroup {
    let shader = stage.shader();
    let entries: Vec<wgpu::BindGroupEntry<'_>> = stage
        .bindings()
        .iter()
        .zip(shader.wgsl.binding_indices.iter())
        .map(|(&res, &binding)| {
            let resource = match res {
                Res::Config => target.config.as_entire_binding(),
                Res::Scene => target.scene.as_entire_binding(),
                Res::Out => wgpu::BindingResource::TextureView(out),
                Res::Gradients => wgpu::BindingResource::TextureView(&target.gradients_view),
                Res::Atlas => wgpu::BindingResource::TextureView(atlas),
                shared => scratch.buffers[shared as usize].as_entire_binding(),
            };
            wgpu::BindGroupEntry { binding: u32::from(binding), resource }
        })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &compiled.layout, entries: &entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_binds_as_many_resources_as_its_shader_declares() {
        for stage in Stage::ALL {
            let shader = stage.shader();
            assert_eq!(
                stage.bindings().len(),
                shader.bindings.len(),
                "{stage:?}: the binding list matches {}",
                shader.name
            );
            for (&res, &kind) in stage.bindings().iter().zip(shader.bindings.iter()) {
                let fits = match res {
                    Res::Config => kind == BindType::Uniform,
                    Res::Out => kind == BindType::Image,
                    Res::Gradients | Res::Atlas => kind == BindType::ImageRead,
                    _ => matches!(kind, BindType::Buffer | BindType::BufReadOnly),
                };
                assert!(fits, "{stage:?}: {res:?} bound where {} declares {kind:?}", shader.name);
            }
        }
    }

    #[test]
    fn the_stages_are_listed_in_their_enum_order() {
        for (at, stage) in Stage::ALL.iter().enumerate() {
            assert_eq!(*stage as usize, at, "Stage::ALL indexes the compiled stages by discriminant");
        }
        for (at, res) in Res::SHARED.iter().enumerate() {
            assert_eq!(*res as usize, at, "Res::SHARED indexes the scratch buffers by discriminant");
            assert!(res.size(&BufferSizes::default()).is_some(), "{res:?} is a shared buffer");
        }
    }

    #[test]
    fn capacities_round_up_to_a_power_of_two_and_never_below_the_minimum() {
        assert_eq!(capacity_for(0), MIN_BUFFER);
        assert_eq!(capacity_for(MIN_BUFFER + 1), 2 * MIN_BUFFER);
        assert_eq!(capacity_for(1 << 20), 1 << 20);
    }
}
