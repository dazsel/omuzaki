//! State every pass and resource shares: the GPU, the bind group layouts the shaders agree on, default resources for
//! empty slots, and the mip generator.
//!
//! Anything a single pass needs lives in that pass's own file; anything two or more need lives here, once.

use crate::environment::{self, GpuEnvironment};
use crate::gpu::Gpu;
use crate::resource::mips::MipGenerator;

/// How many textures a material binds: its base texture and up to four overlays (`limits::MAX_OVERLAYS`).
pub(crate) const MATERIAL_TEXTURES: u32 = 5;

/// The bind group layouts of `shader/common.wgsl`. Group 0 is the frame, group 1 the material; a pass that needs more
/// (shadow maps, a bloom input) appends its own from group 2 (lit passes: shadows at 2, the environment at 3), so passes never disturb one another's layouts.
pub(crate) struct Layouts {
    /// Group 0: frame uniform (b0), instances (b1) and joint matrices (b2).
    pub(crate) frame: wgpu::BindGroupLayout,
    /// Group 1: material uniform (b0), base and overlay textures (b1..=5), their samplers (b6..=10).
    pub(crate) material: wgpu::BindGroupLayout,
    /// Group 0 of the fullscreen post passes: the resolved scene colour (b0) and a bilinear clamp sampler (b1).
    pub(crate) colour_input: wgpu::BindGroupLayout,
    /// Group 2 of the `Shadows` pass: the light's matrix (b0) and nothing else, since the map is its depth target.
    pub(crate) shadow_caster: wgpu::BindGroupLayout,
    /// Group 2 of the scene passes that light surfaces: the light's matrix and lookup settings (b0), the shadow map (b1)
    /// and a comparison sampler (b2).
    pub(crate) shadow_receiver: wgpu::BindGroupLayout,
    /// Group 3 of the scene passes that light surfaces: the environment's uniform (b0), specular cube (b1) and sampler (b2).
    pub(crate) environment: wgpu::BindGroupLayout,
}

/// Stand-ins for texture slots a material leaves empty, so one bind group layout serves every material.
pub(crate) struct Defaults {
    pub(crate) white: wgpu::TextureView,
    pub(crate) sampler: wgpu::Sampler,
    /// Bilinear, clamped: how post passes read the scene colour.
    pub(crate) linear_clamp: wgpu::Sampler,
    /// Depth comparison (`<=`), bilinear, clamped: how the shadow lookup reads the map.
    pub(crate) shadow_compare: wgpu::Sampler,
}

/// Everything shared. See the module docs.
pub(crate) struct Shared {
    pub(crate) gpu: Gpu,
    pub(crate) layouts: Layouts,
    pub(crate) defaults: Defaults,
    pub(crate) mips: MipGenerator,
    /// Group 3 for a scene without an environment: lights nothing.
    pub(crate) no_environment: GpuEnvironment,
}

fn buffer_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    ty: wgpu::BufferBindingType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

impl Layouts {
    fn new(device: &wgpu::Device) -> Self {
        let both = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let storage = wgpu::BufferBindingType::Storage { read_only: true };
        let frame = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[
                buffer_entry(0, both, wgpu::BufferBindingType::Uniform),
                buffer_entry(1, both, storage),
                buffer_entry(2, both, storage),
            ],
        });

        let mut entries = vec![buffer_entry(0, both, wgpu::BufferBindingType::Uniform)];
        entries.extend((0..MATERIAL_TEXTURES).map(|slot| wgpu::BindGroupLayoutEntry {
            binding: 1 + slot,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }));
        entries.extend((0..MATERIAL_TEXTURES).map(|slot| wgpu::BindGroupLayoutEntry {
            binding: 1 + MATERIAL_TEXTURES + slot,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        }));
        let material = device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("material"), entries: &entries });
        let colour_input = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("colour input"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let uniform = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let shadow_caster = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow caster"),
            entries: &[uniform(0, wgpu::ShaderStages::VERTEX)],
        });
        let shadow_receiver = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow receiver"),
            entries: &[
                uniform(0, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        Self { frame, material, colour_input, shadow_caster, shadow_receiver, environment: environment::layout(device) }
    }
}

impl Defaults {
    fn new(gpu: &Gpu) -> Self {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("default white"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255; 4],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        Self {
            white: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            sampler: gpu
                .device
                .create_sampler(&wgpu::SamplerDescriptor { label: Some("default"), ..Default::default() }),
            linear_clamp: gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("linear clamp"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            shadow_compare: gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("shadow compare"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                compare: Some(wgpu::CompareFunction::LessEqual),
                ..Default::default()
            }),
        }
    }
}

impl Shared {
    pub(crate) fn new(gpu: Gpu) -> Self {
        let layouts = Layouts::new(&gpu.device);
        Self {
            no_environment: GpuEnvironment::none(&gpu, &layouts.environment),
            defaults: Defaults::new(&gpu),
            mips: MipGenerator::new(&gpu.device),
            layouts,
            gpu,
        }
    }
}
