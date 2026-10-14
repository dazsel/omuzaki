//! The wgpu renderer: a shadow pass, then one MSAA colour pass that draws the toon meshes, their inverted-hull
//! outlines and the ground shadow catcher, resolving into a texture that egui shows or a screenshot reads back.

use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use wgpu::util::DeviceExt;

use crate::camera::OrbitCamera;
use crate::lighting::Lighting;
use crate::model::{JointTable, Kind, Material, Model, Vertex, Wrap};
use crate::texture;
use crate::timing::GpuTimer;

/// The prototype renders with `antialias: true`, which is 4x MSAA on the canvas.
const MSAA: u32 = 4;
/// Encoded sRGB, like the JS canvas: three.js applies the OETF in the shader and MSAA resolves the encoded values.
/// (egui also requires exactly this format for textures it shows.)
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Shadow map side in texels (the prototype's `mapSize`).
const SHADOW_SIZE: u16 = 1024;
const MAX_LIGHTS: usize = 4;
/// three's shadow `bias` and `normalBias` for the key light in the prototype.
const SHADOW_BIAS: f32 = -0.0004;
const SHADOW_NORMAL_BIAS: f32 = 0.01;
/// Opacity of the ground `ShadowMaterial` in the prototype.
const GROUND_OPACITY: f32 = 0.25;
const GROUND_RADIUS: f32 = 0.8;
const GROUND_SEGMENTS: u32 = 48;

/// What to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderOptions {
    pub outlines: bool,
    pub shadows: bool,
    pub ground: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { outlines: true, shadows: true, ground: true }
    }
}

/// Everything that changes from frame to frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameParams<'a> {
    pub camera: &'a OrbitCamera,
    pub lighting: &'a Lighting,
    pub options: RenderOptions,
}

/// Failures the renderer reports as values.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// Nothing has been rendered yet.
    #[error("no frame rendered yet")]
    NoFrame,
    /// Reading the frame back failed.
    #[error("reading the frame back: {0}")]
    Readback(String),
}

// ---- GPU-side layouts (mirror toon.wgsl) ----

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightUniform {
    dir: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
    view: [[f32; 4]; 4],
    view_proj: [[f32; 4]; 4],
    shadow_view_proj: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    hemi_sky: [f32; 4],
    hemi_ground: [f32; 4],
    lights: [LightUniform; MAX_LIGHTS],
    counts: [u32; 4],
    shadow: [f32; 4],
    post: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialUniform {
    base_color: [f32; 4],
    shade: [f32; 4],
    rim: [f32; 4],
    params: [f32; 4],
    flags: [u32; 4],
    face_rect: [f32; 4],
    highlight: [f32; 4],
    hair: [[f32; 4]; 5],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshUniform {
    joint_base: u32,
    receive_shadow: u32,
    pad: [u32; 2],
    outline: [f32; 4],
}

const OPT_DOUBLE_SIDED: u32 = 1;
const OPT_VERTEX_COLORS: u32 = 2;
const OPT_BROW_ON: u32 = 4;
const OPT_HAS_FACE: u32 = 8;

fn pad4(c: [f32; 3], w: f32) -> [f32; 4] {
    [c[0], c[1], c[2], w]
}

fn material_uniform(m: &Material) -> MaterialUniform {
    let rect = m.face_rect.map_or([0.0, 0.0, 1.0, 1.0], |r| [r.x0, r.y0, 1.0 / (r.x1 - r.x0), 1.0 / (r.y1 - r.y0)]);
    let mut options = 0;
    for (on, bit) in [
        (m.double_sided, OPT_DOUBLE_SIDED),
        (m.vertex_colors, OPT_VERTEX_COLORS),
        (m.brow_on, OPT_BROW_ON),
        (m.face_texture.is_some(), OPT_HAS_FACE),
    ] {
        if on {
            options |= bit;
        }
    }
    MaterialUniform {
        base_color: m.base_color,
        shade: pad4(m.toon.shade, 0.0),
        rim: pad4(m.toon.rim, m.toon.rim_strength),
        params: [m.toon.shift, m.toon.soft, m.toon.rim_power, m.skin_floor],
        flags: [m.hooks.0, options, 0, 0],
        face_rect: rect,
        highlight: pad4(m.highlight, 0.0),
        hair: m.hair_palette.map(|c| pad4(c, 0.0)),
    }
}

// ---- GPU scene ----

struct GpuMaterial {
    bind_group: wgpu::BindGroup,
    pipeline: usize,
}

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    bind_group: wgpu::BindGroup,
    material: usize,
    cast_shadow: bool,
    has_outline: bool,
    double_sided: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct PipelineKey {
    basic: bool,
    cull_back: bool,
    /// `(slope factor bits, units)` of the depth bias.
    bias: Option<(u32, i32)>,
}

struct Layouts {
    global: wgpu::BindGroupLayout,
    global_shadow: wgpu::BindGroupLayout,
    mesh: wgpu::BindGroupLayout,
    material: wgpu::BindGroupLayout,
}

fn buffer_entry(binding: u32, visibility: wgpu::ShaderStages, storage: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: if storage {
                wgpu::BufferBindingType::Storage { read_only: true }
            } else {
                wgpu::BufferBindingType::Uniform
            },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, depth: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: if depth {
                wgpu::TextureSampleType::Depth
            } else {
                wgpu::TextureSampleType::Float { filterable: true }
            },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32, comparison: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(if comparison {
            wgpu::SamplerBindingType::Comparison
        } else {
            wgpu::SamplerBindingType::Filtering
        }),
        count: None,
    }
}

impl Layouts {
    fn new(device: &wgpu::Device) -> Self {
        let both = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let make = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries })
        };
        Self {
            global: make(
                "frame",
                &[
                    buffer_entry(0, both, false),
                    buffer_entry(1, wgpu::ShaderStages::VERTEX, true),
                    texture_entry(2, true),
                    sampler_entry(3, true),
                ],
            ),
            // The shadow pass renders into the shadow map, so its frame group must not bind it for sampling.
            global_shadow: make(
                "frame (shadow pass)",
                &[
                    buffer_entry(0, wgpu::ShaderStages::VERTEX, false),
                    buffer_entry(1, wgpu::ShaderStages::VERTEX, true),
                ],
            ),
            mesh: make("mesh", &[buffer_entry(0, both, false)]),
            material: make(
                "material",
                &[
                    buffer_entry(0, wgpu::ShaderStages::FRAGMENT, false),
                    texture_entry(1, false),
                    sampler_entry(2, false),
                    texture_entry(3, false),
                    texture_entry(4, false),
                    sampler_entry(5, false),
                ],
            ),
        }
    }
}

const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
    0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4, 4 => Uint16x4, 5 => Float32x4, 6 => Float32x2, 7 => Float32x3
];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRIBUTES,
    }
}

struct PipelineSpec<'a> {
    label: &'a str,
    layout: &'a wgpu::PipelineLayout,
    vertex: &'a str,
    /// `None` renders depth only.
    fragment: Option<&'a str>,
    cull: Option<wgpu::Face>,
    bias: wgpu::DepthBiasState,
    blend: Option<wgpu::BlendState>,
    depth_write: bool,
    samples: u32,
}

fn make_pipeline(device: &wgpu::Device, shader: &wgpu::ShaderModule, spec: &PipelineSpec<'_>) -> wgpu::RenderPipeline {
    let targets =
        [Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend: spec.blend, write_mask: wgpu::ColorWrites::ALL })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(spec.label),
        layout: Some(spec.layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(spec.vertex),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(vertex_layout())],
        },
        primitive: wgpu::PrimitiveState { cull_mode: spec.cull, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(spec.depth_write),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: spec.bias,
        }),
        multisample: wgpu::MultisampleState { count: spec.samples, ..Default::default() },
        fragment: spec.fragment.map(|entry| wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

// ---- the renderer ----

fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}

struct Target {
    size: (u32, u32),
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
    resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
}

/// What every pass shares: the frame uniform, the joint matrices and the shadow map.
struct FrameResources {
    buffer: wgpu::Buffer,
    group: wgpu::BindGroup,
    /// The same buffers without the shadow map, for the pass that renders into it.
    group_shadow: wgpu::BindGroup,
    shadow_view: wgpu::TextureView,
}

impl FrameResources {
    fn new(device: &wgpu::Device, layouts: &Layouts, joints: &[Mat4]) -> Self {
        let joint_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("joint matrices"),
            contents: bytemuck::cast_slice(&joints.iter().map(Mat4::to_cols_array_2d).collect::<Vec<_>>()),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame"),
            size: std::mem::size_of::<FrameUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_view = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("shadow map"),
                size: wgpu::Extent3d {
                    width: u32::from(SHADOW_SIZE),
                    height: u32::from(SHADOW_SIZE),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow compare"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame"),
            layout: &layouts.global,
            entries: &[
                entry(0, buffer.as_entire_binding()),
                entry(1, joint_buffer.as_entire_binding()),
                entry(2, wgpu::BindingResource::TextureView(&shadow_view)),
                entry(3, wgpu::BindingResource::Sampler(&shadow_sampler)),
            ],
        });
        let group_shadow = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame (shadow pass)"),
            layout: &layouts.global_shadow,
            entries: &[entry(0, buffer.as_entire_binding()), entry(1, joint_buffer.as_entire_binding())],
        });
        Self { buffer, group, group_shadow, shadow_view }
    }
}

/// Every pipeline the renderer draws with.
struct Pipelines {
    /// One per distinct material draw state, indexed by [`GpuMaterial::pipeline`].
    scene: Vec<wgpu::RenderPipeline>,
    /// Back-face casters (single-sided materials) and both-face casters (double-sided).
    shadow: [wgpu::RenderPipeline; 2],
    outline: wgpu::RenderPipeline,
    ground: wgpu::RenderPipeline,
}

impl Pipelines {
    fn new(device: &wgpu::Device, layouts: &Layouts, keys: &[PipelineKey]) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("toon"),
            source: wgpu::ShaderSource::Wgsl(include_str!("toon.wgsl").into()),
        });
        let layout_of = |label, groups: &[&wgpu::BindGroupLayout]| {
            let groups: Vec<Option<&wgpu::BindGroupLayout>> = groups.iter().map(|g| Some(*g)).collect();
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &groups,
                immediate_size: 0,
            })
        };
        let main = layout_of("toon", &[&layouts.global, &layouts.mesh, &layouts.material]);
        let shadow_layout = layout_of("shadow", &[&layouts.global_shadow, &layouts.mesh]);
        let draw = |label, layout, vertex, fragment, cull, blend, depth_write| PipelineSpec {
            label,
            layout,
            vertex,
            fragment,
            cull,
            bias: wgpu::DepthBiasState::default(),
            blend,
            depth_write,
            samples: MSAA,
        };

        let scene =
            keys.iter()
                .map(|key| {
                    let bias = key.bias.map_or(wgpu::DepthBiasState::default(), |(slope, units)| {
                        wgpu::DepthBiasState { constant: units, slope_scale: f32::from_bits(slope), clamp: 0.0 }
                    });
                    let fragment = if key.basic { "fs_basic" } else { "fs_toon" };
                    let cull = key.cull_back.then_some(wgpu::Face::Back);
                    let spec =
                        PipelineSpec { bias, ..draw("toon", &main, "vs_main", Some(fragment), cull, None, true) };
                    make_pipeline(device, &shader, &spec)
                })
                .collect();
        // three renders shadow casters back-face-first (the opposite of the material's side), so a lit surface
        // never shadows itself; double-sided materials draw both faces.
        let shadow_pipeline = |cull| {
            let spec =
                PipelineSpec { samples: 1, ..draw("shadow", &shadow_layout, "vs_shadow", None, cull, None, true) };
            make_pipeline(device, &shader, &spec)
        };
        let outline = draw("outline", &main, "vs_outline", Some("fs_outline"), Some(wgpu::Face::Front), None, true);
        let ground =
            draw("ground", &main, "vs_main", Some("fs_ground"), None, Some(wgpu::BlendState::ALPHA_BLENDING), false);
        Self {
            scene,
            shadow: [shadow_pipeline(Some(wgpu::Face::Front)), shadow_pipeline(None)],
            outline: make_pipeline(device, &shader, &outline),
            ground: make_pipeline(device, &shader, &ground),
        }
    }
}

/// The distinct draw states the model's materials need, and each material's index into them.
fn pipeline_keys(model: &Model) -> (Vec<PipelineKey>, Vec<usize>) {
    let mut keys: Vec<PipelineKey> = Vec::new();
    let per_material = model
        .materials
        .iter()
        .map(|m| {
            let key = PipelineKey {
                basic: m.kind == Kind::Basic,
                cull_back: !m.double_sided,
                // polygonOffset units are whole numbers in practice (-2, -3).
                #[allow(clippy::cast_possible_truncation)]
                bias: m.polygon_offset.map(|(factor, units)| (factor.to_bits(), units as i32)),
            };
            keys.iter().position(|k| *k == key).unwrap_or_else(|| {
                keys.push(key);
                keys.len() - 1
            })
        })
        .collect();
    (keys, per_material)
}

/// Uploads the model's textures and builds each material's uniform and bind group.
fn build_materials(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    model: &Model,
    layout: &wgpu::BindGroupLayout,
    pipeline_of: &[usize],
) -> Vec<GpuMaterial> {
    let white = texture::white(device, queue);
    let views: Vec<wgpu::TextureView> = model
        .textures
        .iter()
        .enumerate()
        .map(|(i, t)| texture::upload(device, queue, &format!("texture {i}"), t))
        .collect();
    let clamp = texture::sampler(device, Wrap::Clamp, Wrap::Clamp);
    model
        .materials
        .iter()
        .zip(pipeline_of)
        .map(|(m, &pipeline)| {
            let view_of =
                |t: Option<usize>| wgpu::BindingResource::TextureView(t.and_then(|i| views.get(i)).unwrap_or(&white));
            let (u, v) = m
                .base_texture
                .and_then(|i| model.textures.get(i))
                .map_or((Wrap::Repeat, Wrap::Repeat), |t| (t.wrap_s, t.wrap_t));
            let base_sampler = texture::sampler(device, u, v);
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&m.name),
                contents: bytemuck::bytes_of(&material_uniform(m)),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&m.name),
                layout,
                entries: &[
                    entry(0, uniform.as_entire_binding()),
                    entry(1, view_of(m.base_texture)),
                    entry(2, wgpu::BindingResource::Sampler(&base_sampler)),
                    entry(3, view_of(m.face_texture)),
                    entry(4, view_of(m.brow_texture)),
                    entry(5, wgpu::BindingResource::Sampler(&clamp)),
                ],
            });
            GpuMaterial { bind_group, pipeline }
        })
        .collect()
}

/// Uploads the model's meshes in draw order (by `render_order`).
fn build_meshes(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    model: &Model,
    table: &JointTable,
) -> Vec<GpuMesh> {
    let mut order: Vec<usize> = (0..model.meshes.len()).collect();
    order.sort_by_key(|&i| model.meshes[i].render_order);
    order
        .into_iter()
        .map(|i| {
            let mesh = &model.meshes[i];
            upload_mesh(
                device,
                layout,
                &MeshSpec {
                    name: &mesh.name,
                    vertices: &mesh.vertices,
                    indices: &mesh.indices,
                    uniform: MeshUniform {
                        joint_base: table.mesh_base[i],
                        receive_shadow: u32::from(mesh.receive_shadow),
                        pad: [0; 2],
                        outline: mesh.outline.map_or([0.0; 4], |o| pad4(o.color, o.width)),
                    },
                    material: mesh.material,
                    cast_shadow: mesh.cast_shadow,
                    has_outline: mesh.outline.is_some(),
                    double_sided: model.materials.get(mesh.material).is_some_and(|m| m.double_sided),
                },
            )
        })
        .collect()
}

/// Draws a [`Model`] with the toon shading port.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: Pipelines,
    materials: Vec<GpuMaterial>,
    meshes: Vec<GpuMesh>,
    ground: GpuMesh,
    frame: FrameResources,
    target: Option<Target>,
    generation: u64,
    timer: Option<GpuTimer>,
    last_gpu_ms: Option<f32>,
}

impl Renderer {
    /// Uploads `model` and builds every pipeline.
    #[must_use]
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, model: &Model) -> Self {
        let layouts = Layouts::new(device);
        let table = JointTable::build(model, &model.world_matrices());
        let (keys, pipeline_of) = pipeline_keys(model);
        let (ground_vertices, ground_indices) = ground_disc();
        let ground = upload_mesh(
            device,
            &layouts.mesh,
            &MeshSpec {
                name: "ground",
                vertices: &ground_vertices,
                indices: &ground_indices,
                uniform: MeshUniform { joint_base: 0, receive_shadow: 1, pad: [0; 2], outline: [0.0; 4] },
                material: 0,
                cast_shadow: false,
                has_outline: false,
                double_sided: true,
            },
        );
        Self {
            device: device.clone(),
            queue: queue.clone(),
            pipelines: Pipelines::new(device, &layouts, &keys),
            materials: build_materials(device, queue, model, &layouts.material, &pipeline_of),
            meshes: build_meshes(device, &layouts.mesh, model, &table),
            ground,
            frame: FrameResources::new(device, &layouts, &table.matrices),
            target: None,
            generation: 0,
            timer: GpuTimer::new(device, queue),
            last_gpu_ms: None,
        }
    }

    /// The texture the last frame resolved into; egui can show it directly.
    #[must_use]
    pub fn output(&self) -> Option<&wgpu::TextureView> {
        self.target.as_ref().map(|t| &t.resolve_view)
    }

    /// Increments whenever the output texture is recreated, so a caller can re-register it.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// GPU time of the most recent measured frame, when the device supports timestamp queries.
    #[must_use]
    pub fn gpu_ms(&self) -> Option<f32> {
        self.last_gpu_ms
    }

    fn ensure_target(&mut self, size: (u32, u32)) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        let texture = |label, format, samples, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let msaa = texture("colour (msaa)", COLOR_FORMAT, MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let depth = texture("depth", DEPTH_FORMAT, MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let resolve = texture(
            "colour (resolved)",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        self.target =
            Some(Target { size, msaa: view(&msaa), depth: view(&depth), resolve_view: view(&resolve), resolve });
        self.generation += 1;
    }

    fn frame_uniform(size: (u32, u32), params: &FrameParams<'_>) -> FrameUniform {
        let cam = params.camera;
        #[allow(clippy::cast_precision_loss)] // viewport sizes are far below f32's exact integer range
        let aspect = size.0 as f32 / size.1.max(1) as f32;
        let view = cam.view();
        let lighting = params.lighting;
        let mut lights = [LightUniform { dir: [0.0; 4], color: [0.0; 4] }; MAX_LIGHTS];
        for (slot, light) in lights.iter_mut().zip(&lighting.lights) {
            let d = light.direction();
            let c = light.color * light.intensity;
            *slot = LightUniform {
                dir: [d.x, d.y, d.z, f32::from(u8::from(light.casts_shadow))],
                color: [c.x, c.y, c.z, 0.0],
            };
        }
        let p = cam.position();
        let sky = lighting.sky * lighting.hemi_intensity;
        let ground = lighting.ground * lighting.hemi_intensity;
        FrameUniform {
            view: view.to_cols_array_2d(),
            view_proj: (cam.projection(aspect) * view).to_cols_array_2d(),
            shadow_view_proj: lighting.shadow_view_proj().to_cols_array_2d(),
            cam_pos: [p.x, p.y, p.z, 1.0],
            hemi_sky: [sky.x, sky.y, sky.z, 0.0],
            hemi_ground: [ground.x, ground.y, ground.z, 0.0],
            lights,
            counts: [u32::try_from(lighting.lights.len().min(MAX_LIGHTS)).unwrap_or(0), 0, 0, 0],
            shadow: [
                SHADOW_BIAS,
                SHADOW_NORMAL_BIAS,
                1.0 / f32::from(SHADOW_SIZE),
                f32::from(u8::from(params.options.shadows && lighting.shadow_light().is_some())),
            ],
            post: [lighting.exposure, 1.0, GROUND_OPACITY, 0.0],
        }
    }

    /// Renders one frame at `size` pixels into the output texture and submits it.
    pub fn render(&mut self, size: (u32, u32), params: &FrameParams<'_>) {
        let size = (size.0.max(1), size.1.max(1));
        self.ensure_target(size);
        let uniform = Self::frame_uniform(size, params);
        self.queue.write_buffer(&self.frame.buffer, 0, bytemuck::bytes_of(&uniform));
        let shadows_on = uniform.shadow[3] > 0.5;

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        self.shadow_pass(&mut encoder, shadows_on);
        self.scene_pass(&mut encoder, params, shadows_on);
        let measuring = self.timer.as_mut().is_some_and(|t| t.resolve(&mut encoder));
        self.queue.submit([encoder.finish()]);
        if let (true, Some(timer)) = (measuring, self.timer.as_ref()) {
            timer.request_map();
        }
    }

    /// Renders the casters' depth from the key light. Runs (empty) even with shadows off, so the GPU timer always
    /// has its first pass.
    fn shadow_pass(&self, encoder: &mut wgpu::CommandEncoder, enabled: bool) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.frame.shadow_view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: self.timer.as_ref().map(GpuTimer::first_pass),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if !enabled {
            return;
        }
        pass.set_bind_group(0, &self.frame.group_shadow, &[]);
        for mesh in self.meshes.iter().filter(|m| m.cast_shadow) {
            pass.set_pipeline(&self.pipelines.shadow[usize::from(mesh.double_sided)]);
            pass.set_bind_group(1, &mesh.bind_group, &[]);
            mesh.draw(&mut pass);
        }
    }

    /// Renders the toon meshes, their outlines and the ground shadow into the MSAA target, resolving into the output.
    fn scene_pass(&self, encoder: &mut wgpu::CommandEncoder, params: &FrameParams<'_>, shadows_on: bool) {
        let Some(target) = self.target.as_ref() else { return };
        let [r, g, b] = params.lighting.background.map(f64::from);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.msaa,
                depth_slice: None,
                resolve_target: Some(&target.resolve_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &target.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: self.timer.as_ref().map(GpuTimer::last_pass),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &self.frame.group, &[]);
        for mesh in &self.meshes {
            let material = &self.materials[mesh.material];
            pass.set_pipeline(&self.pipelines.scene[material.pipeline]);
            pass.set_bind_group(1, &mesh.bind_group, &[]);
            pass.set_bind_group(2, &material.bind_group, &[]);
            mesh.draw(&mut pass);
        }
        if params.options.outlines {
            pass.set_pipeline(&self.pipelines.outline);
            for mesh in self.meshes.iter().filter(|m| m.has_outline) {
                pass.set_bind_group(1, &mesh.bind_group, &[]);
                pass.set_bind_group(2, &self.materials[mesh.material].bind_group, &[]);
                mesh.draw(&mut pass);
            }
        }
        // The ground draws last: it blends over what is already there. It needs some material's bind group only to
        // satisfy the layout; its shader reads none of it.
        if let (true, true, Some(any)) = (params.options.ground, shadows_on, self.materials.first()) {
            pass.set_pipeline(&self.pipelines.ground);
            pass.set_bind_group(1, &self.ground.bind_group, &[]);
            pass.set_bind_group(2, &any.bind_group, &[]);
            self.ground.draw(&mut pass);
        }
    }

    /// Waits for submitted work and returns the GPU time of the last measured frame, if there is one.
    pub fn finish_timing(&mut self) -> Option<f32> {
        // Errors here mean the device was lost; there is nothing to time then.
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        self.poll_timing()
    }

    /// Collects a finished GPU time measurement without waiting.
    pub fn poll_timing(&mut self) -> Option<f32> {
        // Map callbacks only run when the device is polled; eframe does not poll for us.
        let _ = self.device.poll(wgpu::PollType::Poll);
        if let Some(ms) = self.timer.as_mut().and_then(GpuTimer::collect) {
            self.last_gpu_ms = Some(ms);
        }
        self.last_gpu_ms
    }

    /// Reads the last rendered frame back as tightly packed RGBA8 (sRGB-encoded), with its size.
    ///
    /// # Errors
    /// Fails when nothing has been rendered or the GPU readback fails.
    pub fn read_pixels(&self) -> Result<(Vec<u8>, (u32, u32)), RenderError> {
        let target = self.target.as_ref().ok_or(RenderError::NoFrame)?;
        let (w, h) = target.size;
        let row = w * 4;
        let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.resolve,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            // The receiver outlives the wait below; a send failure just means the reader gave up.
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| RenderError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| RenderError::Readback(e.to_string()))?
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let data = buffer.slice(..).get_mapped_range().map_err(|e| RenderError::Readback(e.to_string()))?;
        let mut pixels = Vec::with_capacity((row * h) as usize);
        for chunk in data.chunks(padded as usize).take(h as usize) {
            pixels.extend_from_slice(&chunk[..row as usize]);
        }
        drop(data);
        buffer.unmap();
        Ok((pixels, (w, h)))
    }
}

impl GpuMesh {
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }
}

/// Everything needed to upload one mesh.
struct MeshSpec<'a> {
    name: &'a str,
    vertices: &'a [Vertex],
    indices: &'a [u32],
    uniform: MeshUniform,
    material: usize,
    cast_shadow: bool,
    has_outline: bool,
    double_sided: bool,
}

fn upload_mesh(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, spec: &MeshSpec<'_>) -> GpuMesh {
    let buffer = |contents: &[u8], usage| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(spec.name), contents, usage })
    };
    let uniform = buffer(bytemuck::bytes_of(&spec.uniform), wgpu::BufferUsages::UNIFORM);
    GpuMesh {
        vertices: buffer(bytemuck::cast_slice(spec.vertices), wgpu::BufferUsages::VERTEX),
        indices: buffer(bytemuck::cast_slice(spec.indices), wgpu::BufferUsages::INDEX),
        index_count: u32::try_from(spec.indices.len()).unwrap_or(0),
        bind_group: device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(spec.name),
            layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        }),
        material: spec.material,
        cast_shadow: spec.cast_shadow,
        has_outline: spec.has_outline,
        double_sided: spec.double_sided,
    }
}

/// The shadow catcher: a flat disc at y = 0 (the prototype's `CircleGeometry(0.8, 48)`).
fn ground_disc() -> (Vec<Vertex>, Vec<u32>) {
    let vertex = |x: f32, z: f32| Vertex {
        position: [x, 0.0, z],
        normal: [0.0, 1.0, 0.0],
        uv: [0.0; 2],
        color: [1.0; 4],
        joints: [0; 4],
        weights: [1.0, 0.0, 0.0, 0.0],
        hair_uv: [0.0; 2],
        hair_info: [0.0; 3],
    };
    let mut vertices = vec![vertex(0.0, 0.0)];
    let mut indices = Vec::new();
    for i in 0..GROUND_SEGMENTS {
        #[allow(clippy::cast_precision_loss)] // segment counts are tiny
        let a = i as f32 / GROUND_SEGMENTS as f32 * std::f32::consts::TAU;
        vertices.push(vertex(GROUND_RADIUS * a.cos(), -GROUND_RADIUS * a.sin()));
        // Counter-clockwise seen from above, so the disc faces up.
        indices.extend([0, 1 + i, 1 + (i + 1) % GROUND_SEGMENTS]);
    }
    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_structs_match_the_wgsl_layout() {
        // 16-byte multiples keep std140 happy; the sizes are the sums of the WGSL members.
        assert_eq!(std::mem::size_of::<MeshUniform>(), 32);
        assert_eq!(std::mem::size_of::<MaterialUniform>(), 16 * 12);
        assert_eq!(std::mem::size_of::<FrameUniform>(), 3 * 64 + 3 * 16 + MAX_LIGHTS * 32 + 3 * 16);
    }

    #[test]
    fn ground_disc_faces_up() {
        let (v, i) = ground_disc();
        for tri in i.chunks(3) {
            let p = |k: usize| glam::Vec3::from(v[tri[k] as usize].position);
            let n = (p(1) - p(0)).cross(p(2) - p(0));
            assert!(n.y > 0.0, "{n}");
        }
    }
}
