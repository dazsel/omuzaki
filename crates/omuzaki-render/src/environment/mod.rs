//! Environments: image-based light (LK-08). `create_environment` bakes the sky once ([`bake`]) and uploads what a frame
//! reads: nine harmonics for the diffuse light and a prefiltered specular cube. A frame then writes one small uniform
//! (intensity, rotation, toon ramp) and the lit passes bind the environment at group 3; nothing is filtered or allocated
//! per frame.
//!
//! The shader side is `shader/environment.wgsl`. Without an environment in the scene, passes bind [`GpuEnvironment::none`],
//! an all-zero stand-in that lights nothing.

pub(crate) mod backdrop;
mod bake;
mod half;

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::{Mat3, Vec3};
use omuzaki_render_api::{EnvironmentDesc, EnvironmentUse, RenderError};

use crate::gpu::Gpu;
use crate::shared::Layouts;
use backdrop::GpuBackdrop;
use bake::{Baked, SPECULAR_FACE, SPECULAR_LEVELS};

/// Mirrors `struct Environment` in `shader/environment.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct EnvironmentUniform {
    sh: [[f32; 4]; 9],
    /// The columns of the rotation from world to sky.
    to_sky: [[f32; 4]; 3],
    /// xyz: the brightest direction; `w`: 1 with the toon ramp.
    toward: [f32; 4],
    dark: [f32; 4],
    bright: [f32; 4],
    /// `x`: intensity; `y`: the last specular level.
    params: [f32; 4],
}

/// An environment on the GPU.
pub(crate) struct GpuEnvironment {
    /// What does not change: the harmonics and the toon split, with the per-frame fields at their defaults.
    base: EnvironmentUniform,
    uniform: wgpu::Buffer,
    /// Kept alive by the bind group; named so `bytes` stays honest about what it counts.
    _cube: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// The sky as a [`Background::Environment`](omuzaki_render_api::Background) shows it.
    backdrop: GpuBackdrop,
    /// Estimated GPU memory, for `ResourceStats`.
    pub(crate) bytes: u64,
}

/// Who reads group 3. Compute is for the shader tests, which evaluate the lighting functions without a draw.
const STAGES: wgpu::ShaderStages = wgpu::ShaderStages::FRAGMENT.union(wgpu::ShaderStages::COMPUTE);

/// The bind group layout of group 3: the uniform, the specular cube and its sampler.
pub(crate) fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("environment"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: STAGES,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: STAGES,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: STAGES,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// Bytes of a cube chain of `levels` levels, `face` texels at the base, in RGBA16F, with every step checked.
fn cube_bytes(face: u32, levels: u32) -> Result<u64, RenderError> {
    const TEXEL: u64 = 8;
    let overflow = || RenderError::LimitExceeded { what: "environment size", limit: u64::MAX };
    (0..levels).try_fold(0_u64, |total, level| {
        let side = u64::from((face >> level).max(1));
        side.checked_mul(side)
            .and_then(|texels| texels.checked_mul(6 * TEXEL))
            .and_then(|bytes| total.checked_add(bytes))
            .ok_or_else(overflow)
    })
}

impl GpuEnvironment {
    /// Bakes `desc` and uploads it. The size limit is the caller's to check (`WgpuRenderer::create_environment`).
    ///
    /// # Errors
    /// `Invalid` per [`EnvironmentDesc::validate`], `LimitExceeded` if a size computation overflows.
    pub(crate) fn create(gpu: &Gpu, layouts: &Layouts, desc: &EnvironmentDesc) -> Result<Self, RenderError> {
        let Baked { diffuse, specular } = bake::bake(desc)?;
        let backdrop = GpuBackdrop::new(gpu, &layouts.backdrop, Some(desc));
        let bytes = cube_bytes(SPECULAR_FACE, SPECULAR_LEVELS)?
            .checked_add(size_of::<EnvironmentUniform>() as u64)
            .and_then(|b| b.checked_add(backdrop.bytes))
            .ok_or(RenderError::LimitExceeded { what: "environment size", limit: u64::MAX })?;
        let base = EnvironmentUniform {
            sh: diffuse.packed,
            to_sky: [[0.0; 4]; 3],
            toward: [diffuse.toward.x, diffuse.toward.y, diffuse.toward.z, 0.0],
            dark: [diffuse.dark.x, diffuse.dark.y, diffuse.dark.z, 0.0],
            bright: [diffuse.bright.x, diffuse.bright.y, diffuse.bright.z, 0.0],
            #[allow(clippy::cast_precision_loss)] // a handful of levels
            params: [0.0, (SPECULAR_LEVELS - 1) as f32, 0.0, 0.0],
        };
        let cube = upload_cube(gpu, SPECULAR_FACE, &specular.levels);
        Ok(Self::assemble(gpu, &layouts.environment, base, cube, backdrop, bytes))
    }

    /// The stand-in for a scene without an environment: lights nothing, reads a black cube, shows black.
    pub(crate) fn none(gpu: &Gpu, layouts: &Layouts) -> Self {
        let black = vec![vec![0.0_f32; 6 * 4]];
        let cube = upload_cube(gpu, 1, &black);
        let backdrop = GpuBackdrop::new(gpu, &layouts.backdrop, None);
        Self::assemble(gpu, &layouts.environment, EnvironmentUniform::zeroed(), cube, backdrop, 0)
    }

    fn assemble(
        gpu: &Gpu,
        layout: &wgpu::BindGroupLayout,
        base: EnvironmentUniform,
        cube: wgpu::Texture,
        backdrop: GpuBackdrop,
        bytes: u64,
    ) -> Self {
        let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("environment"),
            size: size_of::<EnvironmentUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&base));
        let view = cube.create_view(&wgpu::TextureViewDescriptor {
            label: Some("environment"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("environment"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("environment"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        Self { base, uniform, _cube: cube, bind_group, backdrop, bytes }
    }

    /// The sky as a backdrop shows it.
    pub(crate) fn backdrop(&self) -> &GpuBackdrop {
        &self.backdrop
    }

    /// Group 3 for the passes that light with this environment.
    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Writes how this frame uses the environment: 256 bytes into a buffer that already exists.
    pub(crate) fn apply(&self, queue: &wgpu::Queue, using: &EnvironmentUse) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.uniform_for(using)));
    }

    /// The uniform for one frame's use: the baked fields plus intensity, the turn from world to sky and the ramp flag.
    fn uniform_for(&self, using: &EnvironmentUse) -> EnvironmentUniform {
        let to_sky = to_sky(using);
        let column = |v: Vec3| [v.x, v.y, v.z, 0.0];
        let mut uniform = self.base;
        uniform.to_sky = [column(to_sky.x_axis), column(to_sky.y_axis), column(to_sky.z_axis)];
        uniform.toward[3] = f32::from(u8::from(using.toon_ramp));
        uniform.params[0] = using.intensity;
        uniform
    }
}

/// The turn from a world direction to the sky's frame: turning the sky by +θ about Y is turning every lookup direction
/// by -θ.
pub(crate) fn to_sky(using: &EnvironmentUse) -> Mat3 {
    Mat3::from_rotation_y(-using.rotation_deg.to_radians())
}

/// A filterable `Rgba16Float` cube of `levels.len()` levels whose base side is `face`, from RGBA `f32` texels (six faces
/// per level, in the order `bake::face_direction` uses).
fn upload_cube(gpu: &Gpu, face: u32, levels: &[Vec<f32>]) -> wgpu::Texture {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("environment specular"),
        size: wgpu::Extent3d { width: face, height: face, depth_or_array_layers: 6 },
        mip_level_count: u32::try_from(levels.len()).unwrap_or(1),
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    for (level, texels) in (0_u32..).zip(levels) {
        let side = (face >> level).max(1);
        let bytes: Vec<u8> = texels.iter().flat_map(|&v| half::half_bits(v).to_le_bytes()).collect();
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(side * 8), rows_per_image: Some(side) },
            wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 6 },
        );
    }
    texture
}

#[cfg(test)]
mod tests;
