//! Textures: RGBA8 pictures the materials sample, including the dynamic ones painters draw into.
//!
//! **Storage.** The contract lets a texture be sRGB or linear data, with straight or premultiplied alpha. Straight sRGB
//! colour (the common case) uses an `Rgba8UnormSrgb` texture, so the hardware decodes before it filters. Everything
//! else is stored as plain `Rgba8Unorm` and the shader finishes the job after filtering (divide by alpha, decode), the
//! same steps as the mock's `sample`. [`storage`] is the one place that decides, and `shader/common.wgsl`
//! (`texel`) the one place that undoes it.
//!
//! **Dynamic textures** are painted by GPU painters that draw into a storage texture and write straight alpha (Vello),
//! so a dynamic straight-alpha texture is two textures (`resource/premultiply.rs`): the painter's `Rgba8Unorm` storage
//! texture, and the sampled one, premultiplied in linear light with sRGB hardware encoding and a mip chain. Materials
//! only ever see the sampled one, so they get the normal sampler (anisotropy, wrap, trilinear) with correct edges.

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, ColourSpace, Filter, PixelRect, Sampler, TextureDesc, TextureUsage, Wrap};

use crate::gpu::Gpu;
use crate::resource::mips::{MipGenerator, MipStep};
use crate::resource::premultiply::PremultiplyStep;

/// Bit for `texture flags` in the material uniform: the shader must sRGB-decode what it samples.
pub(crate) const DECODE_SRGB: u32 = 1;
/// Bit for `texture flags` in the material uniform: the shader must divide what it samples by alpha.
pub(crate) const PREMULTIPLIED: u32 = 2;

/// Whether `desc` is a straight-alpha texture a painter writes: those get a separate premultiplied sampled texture.
fn needs_premultiply(desc: &TextureDesc) -> bool {
    desc.usage == TextureUsage::Dynamic && desc.alpha == AlphaEncoding::Straight
}

/// The format of the texture the materials sample for `desc`, and the shader flags that finish the decoding.
fn storage(desc: &TextureDesc) -> (wgpu::TextureFormat, u32) {
    if needs_premultiply(desc) {
        // Premultiplied in linear light by `premultiply.wgsl`; the sRGB format encodes it, the sampler decodes it.
        let format = match desc.colour_space {
            ColourSpace::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            ColourSpace::Linear => wgpu::TextureFormat::Rgba8Unorm,
        };
        return (format, PREMULTIPLIED);
    }
    match (desc.colour_space, desc.alpha) {
        (ColourSpace::Srgb, AlphaEncoding::Straight) => (wgpu::TextureFormat::Rgba8UnormSrgb, 0),
        (ColourSpace::Srgb, AlphaEncoding::Premultiplied) => {
            (wgpu::TextureFormat::Rgba8Unorm, DECODE_SRGB | PREMULTIPLIED)
        }
        (ColourSpace::Linear, AlphaEncoding::Straight) => (wgpu::TextureFormat::Rgba8Unorm, 0),
        (ColourSpace::Linear, AlphaEncoding::Premultiplied) => (wgpu::TextureFormat::Rgba8Unorm, PREMULTIPLIED),
    }
}

/// Levels in a full chain for `size`: down to 1x1.
fn mip_levels(size: UVec2) -> u32 {
    size.x.max(size.y).bit_width()
}

fn sampler(gpu: &Gpu, s: Sampler, mipped: bool) -> wgpu::Sampler {
    let (filter, mipmap, anisotropy) = match s.filter {
        Filter::Linear => (
            wgpu::FilterMode::Linear,
            wgpu::MipmapFilterMode::Linear,
            u16::from(s.anisotropy).min(gpu.max_anisotropy()),
        ),
        // Anisotropic filtering needs linear filtering everywhere, so it is off for hard texels.
        Filter::Nearest => (wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Nearest, 1),
    };
    let address = match s.wrap {
        Wrap::Clamp => wgpu::AddressMode::ClampToEdge,
        Wrap::Repeat => wgpu::AddressMode::Repeat,
    };
    gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("texture"),
        address_mode_u: address,
        address_mode_v: address,
        address_mode_w: address,
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: mipmap,
        // Without mips only level 0 exists; clamping the LOD keeps the sampler honest about that.
        lod_max_clamp: if mipped { 32.0 } else { 0.0 },
        anisotropy_clamp: anisotropy,
        ..Default::default()
    })
}

/// A texture on the GPU.
pub(crate) struct GpuTexture {
    pub(crate) desc: TextureDesc,
    /// What painters and uploads write: the texture itself, or for a straight-alpha dynamic one the `Rgba8Unorm` storage
    /// texture (the sampled texture is then a different one).
    pub(crate) texture: wgpu::Texture,
    /// What the materials sample (a view of the sampled texture).
    pub(crate) view: wgpu::TextureView,
    pub(crate) sampler: wgpu::Sampler,
    /// [`DECODE_SRGB`] and [`PREMULTIPLIED`], for the shader.
    pub(crate) shader_flags: u32,
    /// Format of the sampled texture, which picks the mip pipeline.
    pub(crate) format: wgpu::TextureFormat,
    /// The texture the materials sample (the same as `texture` unless `premultiply` is set), for readback in tests.
    #[cfg(test)]
    pub(crate) sampled: wgpu::Texture,
    /// Present for a straight-alpha dynamic texture: the sampled texture and the step that fills it.
    pub(crate) premultiply: Option<PremultiplyStep>,
    /// The blits that rebuild the chain after a paint. Kept only for dynamic textures.
    pub(crate) mip_steps: Vec<MipStep>,
    pub(crate) bytes: u64,
}

impl GpuTexture {
    /// Creates the texture, uploads `rgba` if given and builds its sampled picture (premultiply and mip chain).
    pub(crate) fn create(gpu: &Gpu, mips: &MipGenerator, desc: &TextureDesc, rgba: Option<&[u8]>) -> Self {
        let (format, shader_flags) = storage(desc);
        let levels = if desc.mips { mip_levels(desc.size) } else { 1 };
        let make = |format, usage, mip_level_count| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("texture"),
                size: wgpu::Extent3d { width: desc.size.x, height: desc.size.y, depth_or_array_layers: 1 },
                mip_level_count,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        // Render attachment for the mip blits and for raster GPU painters (the `native()` convention); copy source so a
        // level can be read back for tests and tools.
        let sampled_usage = wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::RENDER_ATTACHMENT;
        let writable = sampled_usage | wgpu::TextureUsages::COPY_DST;
        let (texture, sampled, premultiply) = if needs_premultiply(desc) {
            // The painter's texture is `Rgba8Unorm` with `STORAGE_BINDING`: an sRGB format cannot be a storage texture.
            let painter = make(wgpu::TextureFormat::Rgba8Unorm, writable | wgpu::TextureUsages::STORAGE_BINDING, 1);
            let sampled = make(format, sampled_usage, levels);
            let step = mips.premultiplier.step(&gpu.device, &painter, &sampled);
            (painter, sampled, Some(step))
        } else {
            // A dynamic premultiplied texture is already `Rgba8Unorm`, so it takes storage binding as well.
            let storage = if desc.usage == TextureUsage::Dynamic {
                wgpu::TextureUsages::STORAGE_BINDING
            } else {
                wgpu::TextureUsages::empty()
            };
            let texture = make(format, writable | storage, levels);
            (texture.clone(), texture, None)
        };
        let copies = if premultiply.is_some() { 2 } else { 1 };
        let this = Self {
            desc: *desc,
            view: sampled.create_view(&wgpu::TextureViewDescriptor::default()),
            sampler: sampler(gpu, desc.sampler, levels > 1),
            shader_flags,
            format,
            premultiply,
            mip_steps: mips.steps(&gpu.device, &sampled, levels),
            #[cfg(test)]
            sampled,
            bytes: copies * if levels > 1 { desc.byte_len() as u64 * 4 / 3 } else { desc.byte_len() as u64 },
            texture,
        };
        if let Some(rgba) = rgba {
            this.write(gpu, PixelRect::full(desc.size), rgba);
        }
        if levels > 1 || this.premultiply.is_some() {
            mips.refresh_now(gpu, &this);
        }
        this.settle()
    }

    /// A static texture never needs its blits again; drop them rather than hold them for its lifetime.
    fn settle(mut self) -> Self {
        if self.desc.usage == TextureUsage::Static {
            self.mip_steps = Vec::new();
        }
        self
    }

    /// Copies `rgba` (tightly packed, `rect.byte_len()` bytes) into `rect` of level 0.
    pub(crate) fn write(&self, gpu: &Gpu, rect: PixelRect, rgba: &[u8]) {
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: rect.x, y: rect.y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(rect.width * 4),
                rows_per_image: Some(rect.height),
            },
            wgpu::Extent3d { width: rect.width, height: rect.height, depth_or_array_layers: 1 },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chains_reach_one_pixel() {
        assert_eq!(mip_levels(UVec2::new(1, 1)), 1);
        assert_eq!(mip_levels(UVec2::new(2, 2)), 2);
        assert_eq!(mip_levels(UVec2::new(1536, 1024)), 11, "1536 needs 11 halvings to reach 1");
        assert_eq!(mip_levels(UVec2::new(4096, 3)), 13);
    }

    #[test]
    fn only_static_straight_srgb_uses_hardware_decoding() {
        let mut desc = TextureDesc::colour(UVec2::new(4, 4));
        assert_eq!(storage(&desc), (wgpu::TextureFormat::Rgba8UnormSrgb, 0));
        desc.alpha = AlphaEncoding::Premultiplied;
        assert_eq!(storage(&desc), (wgpu::TextureFormat::Rgba8Unorm, DECODE_SRGB | PREMULTIPLIED));
        desc.colour_space = ColourSpace::Linear;
        assert_eq!(storage(&desc), (wgpu::TextureFormat::Rgba8Unorm, PREMULTIPLIED));
        desc.alpha = AlphaEncoding::Straight;
        assert_eq!(storage(&desc), (wgpu::TextureFormat::Rgba8Unorm, 0));
    }

    #[test]
    fn dynamic_straight_textures_are_sampled_premultiplied_in_the_hardware_format() {
        let mut desc = TextureDesc::colour(UVec2::new(4, 4)).dynamic();
        assert!(needs_premultiply(&desc));
        assert_eq!(storage(&desc), (wgpu::TextureFormat::Rgba8UnormSrgb, PREMULTIPLIED));
        desc.colour_space = ColourSpace::Linear;
        assert_eq!(storage(&desc), (wgpu::TextureFormat::Rgba8Unorm, PREMULTIPLIED));
        assert!(!needs_premultiply(&TextureDesc::colour(UVec2::new(4, 4))), "a static texture is never painted");
        desc.alpha = AlphaEncoding::Premultiplied;
        assert!(!needs_premultiply(&desc), "premultiplied painters already write what is sampled");
    }
}
