//! Texture upload: sRGB RGBA8 images with a CPU-built mip chain, and the samplers that read them.

use crate::model::{Texture, Wrap};

fn srgb_to_linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    // Rounded and clamped to 0..=255 first, so the cast cannot truncate or wrap.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = (s * 255.0).round() as u8;
    byte
}

/// Halves an sRGB RGBA8 image with a 2x2 box filter in linear light (what GL's `generateMipmap` does for sRGB
/// textures); alpha is averaged as stored.
fn downsample(rgba: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let lut: Vec<f32> = (0..=255).map(srgb_to_linear).collect();
    let at = |x: u32, y: u32, c: usize| rgba[((y.min(h - 1) * w + x.min(w - 1)) as usize) * 4 + c];
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let taps = [(2 * x, 2 * y), (2 * x + 1, 2 * y), (2 * x, 2 * y + 1), (2 * x + 1, 2 * y + 1)];
            for c in 0..3 {
                out.push(linear_to_srgb8(
                    taps.iter().map(|&(tx, ty)| lut[usize::from(at(tx, ty, c))]).sum::<f32>() / 4.0,
                ));
            }
            let alpha: u32 = taps.iter().map(|&(tx, ty)| u32::from(at(tx, ty, 3))).sum();
            out.push(u8::try_from((alpha + 2) / 4).unwrap_or(255));
        }
    }
    (out, nw, nh)
}

/// Uploads `texture` as an sRGB texture with a full mip chain and returns a view of it.
pub(crate) fn upload(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, texture: &Texture) -> wgpu::TextureView {
    let mut level = texture.rgba.clone();
    let (mut w, mut h) = (texture.width, texture.height);
    let mip_count = w.max(h).ilog2() + 1;
    let gpu = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: mip_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for mip in 0..mip_count {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &level,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        if mip + 1 < mip_count {
            (level, w, h) = downsample(&level, w, h);
        }
    }
    gpu.create_view(&wgpu::TextureViewDescriptor::default())
}

/// A 1x1 white texture, bound where a material has no image so the shader can sample unconditionally.
pub(crate) fn white(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let pixel = Texture { width: 1, height: 1, rgba: vec![255; 4], wrap_s: Wrap::Clamp, wrap_t: Wrap::Clamp };
    upload(device, queue, "white", &pixel)
}

fn address_mode(wrap: Wrap) -> wgpu::AddressMode {
    match wrap {
        Wrap::Repeat => wgpu::AddressMode::Repeat,
        Wrap::Mirror => wgpu::AddressMode::MirrorRepeat,
        Wrap::Clamp => wgpu::AddressMode::ClampToEdge,
    }
}

/// A trilinear, 8x anisotropic sampler (the prototype sets anisotropy 8 on its canvas textures).
pub(crate) fn sampler(device: &wgpu::Device, u: Wrap, v: Wrap) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("texture sampler"),
        address_mode_u: address_mode(u),
        address_mode_v: address_mode(v),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        anisotropy_clamp: 8,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_keeps_flat_colours_and_halves_the_size() {
        let (out, w, h) = downsample(&[200, 100, 50, 255].repeat(16), 4, 4);
        assert_eq!((w, h), (2, 2));
        assert_eq!(&out[..4], &[200, 100, 50, 255]);
    }

    #[test]
    fn downsample_averages_in_linear_light() {
        // Black and white average to sRGB 188 (linear 0.5), not 128.
        let (out, ..) = downsample(&[0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255], 2, 2);
        assert!((i32::from(out[0]) - 188).abs() <= 1, "{}", out[0]);
    }

    #[test]
    fn downsample_handles_odd_and_thin_images() {
        let (out, w, h) = downsample(&[10, 20, 30, 255].repeat(3), 3, 1);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out.len(), 4);
    }
}
