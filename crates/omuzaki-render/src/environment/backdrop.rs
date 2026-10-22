//! The environment seen behind the scene ([`Background::Environment`](omuzaki_render_api::Background), LK-08): the
//! sky's radiance as an equirectangular texture, read at full resolution in each pixel's view direction by
//! `shader/backdrop.wgsl`.
//!
//! The lighting reads the baked harmonics and the 64-texel specular cube, which are far too coarse to look at, so the
//! backdrop keeps its own picture: an HDRI as given (`Rgba16Float`), a procedural sky drawn on [`SKY_ROWS`] rows, which
//! is exact for a sky whose colour depends on elevation only. It is drawn by the `Opaque` pass after the opaque
//! surfaces, at the far plane, where nothing was drawn.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::{Mat3, UVec2};
use omuzaki_render_api::{EnvironmentDesc, sky_radiance};

use super::half::half_bits;
use crate::gpu::Gpu;

/// Rows of the texture a procedural sky is drawn on: one per 0.35° of elevation, finer than any picture shows.
const SKY_ROWS: u32 = 512;
/// Columns of that texture: the sky doesn't change around the horizon, so two are enough for the filter.
const SKY_COLUMNS: u32 = 2;

/// Mirrors `struct Backdrop` in `shader/backdrop.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct BackdropUniform {
    /// The columns of the matrix from normalised device coordinates `(x, y, 1)` to a direction in the sky's frame.
    rays: [[f32; 4]; 3],
    /// `x`: the intensity.
    params: [f32; 4],
}

impl BackdropUniform {
    /// `rays` (see `Camera::sky_rays`) already turned into the sky's frame, and the intensity.
    pub(crate) fn new(rays: Mat3, intensity: f32) -> Self {
        let column = |v: omuzaki_core::glam::Vec3| [v.x, v.y, v.z, 0.0];
        Self {
            rays: [column(rays.x_axis), column(rays.y_axis), column(rays.z_axis)],
            params: [intensity, 0.0, 0.0, 0.0],
        }
    }
}

/// The bind group layout of the backdrop draw's group 0: the uniform, the sky texture and its sampler.
pub(crate) fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let fragment = wgpu::ShaderStages::FRAGMENT;
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("backdrop"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: fragment,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: fragment,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: fragment,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// The size and linear RGBA texels of the picture `desc` shows as a backdrop.
fn picture(desc: &EnvironmentDesc) -> (UVec2, Vec<f32>) {
    match desc {
        EnvironmentDesc::Sky { zenith, horizon, ground } => {
            let mut texels = Vec::with_capacity((SKY_ROWS * SKY_COLUMNS * 4) as usize);
            for row in 0..SKY_ROWS {
                // Row centres from straight up to straight down: v = acos(y) / π, so y = cos(π v).
                #[allow(clippy::cast_precision_loss)] // a few hundred rows
                let v = (row as f32 + 0.5) / SKY_ROWS as f32;
                let c = sky_radiance(*zenith, *horizon, *ground, (std::f32::consts::PI * v).cos());
                for _ in 0..SKY_COLUMNS {
                    texels.extend_from_slice(&[c.x, c.y, c.z, 1.0]);
                }
            }
            (UVec2::new(SKY_COLUMNS, SKY_ROWS), texels)
        }
        EnvironmentDesc::Hdri { size, rgb } => {
            let texels = rgb.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 1.0]).collect();
            (*size, texels)
        }
    }
}

/// An environment's backdrop on the GPU.
pub(crate) struct GpuBackdrop {
    uniform: wgpu::Buffer,
    /// Kept alive by the bind group.
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// GPU memory of the texture and the uniform, for `ResourceStats`.
    pub(crate) bytes: u64,
}

impl GpuBackdrop {
    /// Uploads the picture of `desc`; `None` (the stand-in environment) uploads one black texel.
    pub(crate) fn new(gpu: &Gpu, layout: &wgpu::BindGroupLayout, desc: Option<&EnvironmentDesc>) -> Self {
        let (size, texels) = desc.map_or_else(|| (UVec2::ONE, vec![0.0, 0.0, 0.0, 1.0]), picture);
        let extent = wgpu::Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("environment backdrop"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes: Vec<u8> = texels.iter().flat_map(|&v| half_bits(v).to_le_bytes()).collect();
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size.x * 8), rows_per_image: Some(size.y) },
            extent,
        );
        let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("environment backdrop"),
            size: size_of::<BackdropUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Around the horizon the picture wraps; at the poles it stops.
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("environment backdrop"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("environment backdrop"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let bytes = u64::from(size.x) * u64::from(size.y) * 8 + size_of::<BackdropUniform>() as u64;
        Self { uniform, _texture: texture, bind_group, bytes }
    }

    /// Writes this frame's rays and intensity.
    pub(crate) fn write(&self, queue: &wgpu::Queue, uniform: &BackdropUniform) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(uniform));
    }

    /// The backdrop draw's group 0.
    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_render_api::Colour;

    use super::*;

    #[test]
    fn a_sky_is_drawn_from_zenith_to_ground_down_its_rows() {
        let desc =
            EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::hex(0x0080_8080), ground: Colour::BLACK };
        let (size, texels) = picture(&desc);
        assert_eq!(size, UVec2::new(SKY_COLUMNS, SKY_ROWS));
        let row = |r: u32| texels[(r * SKY_COLUMNS * 4) as usize];
        assert!(row(0) > 0.99 && row(SKY_ROWS - 1) < 0.01, "white at the top, black at the bottom");
        let horizon = Colour::hex(0x0080_8080).to_linear().x;
        assert!((row(SKY_ROWS / 2) - horizon).abs() < 0.01, "the horizon in the middle row");
    }

    #[test]
    fn an_hdri_keeps_its_pixels() {
        let desc = EnvironmentDesc::Hdri { size: UVec2::new(2, 1), rgb: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0] };
        assert_eq!(picture(&desc), (UVec2::new(2, 1), vec![1.0, 2.0, 3.0, 1.0, 4.0, 5.0, 6.0, 1.0]));
    }
}
