//! Mip chains, built on the GPU with one box-filter blit per level.
//!
//! The chain is rebuilt after a static texture is created and after every paint of a dynamic one (the `PaintTextures`
//! pass), only over the rectangle that was painted. Blitting happens in the texture's own format, so an sRGB texture is
//! averaged in linear light (the hardware decodes on read and encodes on write) and a premultiplied one is averaged as
//! stored, which is exactly what premultiplied data needs to avoid dark fringes. Odd sizes keep their last row and column
//! (`shader/mips.wgsl`).
//!
//! [`MipGenerator::refresh`] is the one entry for keeping a texture's sampled picture current: for a dynamic
//! straight-alpha texture it first premultiplies the repainted rectangle ([`Premultiplier`]), then rebuilds the levels.

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::PixelRect;

use crate::gpu::Gpu;
use crate::resource::premultiply::Premultiplier;
use crate::resource::texture::GpuTexture;

/// Vertices of the oversized triangle that covers the screen (`vs_main` of the blit shaders).
pub(crate) const FULLSCREEN_TRIANGLE_VERTICES: u32 = 3;

/// The two formats textures are sampled in (see `texture::storage`).
const FORMATS: [wgpu::TextureFormat; 2] = [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb];

/// What one blit needs: the level to draw into and a bind group reading the level above it.
pub(crate) struct MipStep {
    destination: wgpu::TextureView,
    source: wgpu::BindGroup,
    source_size: UVec2,
    destination_size: UVec2,
}

/// Builds mip chains and keeps sampled pictures current. One per renderer; the pipelines are made up front because a
/// paint must not compile shaders.
pub(crate) struct MipGenerator {
    pipelines: [wgpu::RenderPipeline; 2],
    layout: wgpu::BindGroupLayout,
    pub(crate) premultiplier: Premultiplier,
}

impl MipGenerator {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mips"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shader/mips.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mips"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mips"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mips"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self { pipelines: FORMATS.map(pipeline), layout, premultiplier: Premultiplier::new(device) }
    }

    /// The blits that fill levels `1..levels` of `texture`, built once per texture so repainting allocates nothing.
    pub(crate) fn steps(&self, device: &wgpu::Device, texture: &wgpu::Texture, levels: u32) -> Vec<MipStep> {
        let level_view = |level| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let base = UVec2::new(texture.width(), texture.height());
        // Each level is half the one above, rounded down, never below one pixel.
        let size = |level: u32| UVec2::new((base.x >> level).max(1), (base.y >> level).max(1));
        (1..levels)
            .map(|level| MipStep {
                source: device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mips"),
                    layout: &self.layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&level_view(level - 1)),
                    }],
                }),
                destination: level_view(level),
                source_size: size(level - 1),
                destination_size: size(level),
            })
            .collect()
    }

    /// Records the blits for a texture of `format` into `encoder`, over `dirty` (pixels of level 0). A level is redrawn
    /// where its pixels read something that changed: the rectangle is carried down the chain, rounded outward.
    pub(crate) fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        format: wgpu::TextureFormat,
        steps: &[MipStep],
        dirty: PixelRect,
    ) {
        let Some(index) = FORMATS.iter().position(|&f| f == format) else { return };
        let (mut x, mut y) = ((dirty.x, dirty.x + dirty.width), (dirty.y, dirty.y + dirty.height));
        for step in steps {
            x = carry(x, step.source_size.x, step.destination_size.x);
            y = carry(y, step.source_size.y, step.destination_size.y);
            if x.0 >= x.1 || y.0 >= y.1 {
                return; // nothing changed at this level, so nothing changes below it
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mip level"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &step.destination,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipelines[index]);
            pass.set_bind_group(0, &step.source, &[]);
            pass.set_scissor_rect(x.0, y.0, x.1 - x.0, y.1 - y.0);
            pass.draw(0..FULLSCREEN_TRIANGLE_VERTICES, 0..1);
        }
    }

    /// Brings `texture`'s sampled picture up to date over `dirty` (pixels of level 0): premultiplies the rectangle if the
    /// texture is a straight-alpha painter target, then rebuilds the mip levels under it.
    pub(crate) fn refresh(&self, encoder: &mut wgpu::CommandEncoder, texture: &GpuTexture, dirty: PixelRect) {
        if let Some(step) = &texture.premultiply {
            self.premultiplier.record(encoder, step, dirty);
        }
        self.record(encoder, texture.format, &texture.mip_steps, dirty);
    }

    /// [`refresh`](Self::refresh) over the whole texture, in a command buffer of its own (for a texture that was just
    /// created or written outside a frame).
    pub(crate) fn refresh_now(&self, gpu: &Gpu, texture: &GpuTexture) {
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("mips") });
        self.refresh(&mut encoder, texture, PixelRect::full(texture.desc.size));
        gpu.queue.submit([encoder.finish()]);
    }
}

/// The destination pixels `lo..hi` (of a level `to` pixels across) that read source pixels in `lo..hi` (of a level `from`
/// across): pixel `p` reads the source range `p * from / to .. (p + 1) * from / to`.
fn carry((lo, hi): (u32, u32), from: u32, to: u32) -> (u32, u32) {
    let (from, to) = (u64::from(from), u64::from(to));
    let first = u64::from(lo) * to / from;
    let end = (u64::from(hi) * to).div_ceil(from).min(to);
    // Both are at most `to`, a u32.
    (u32::try_from(first).unwrap_or(u32::MAX), u32::try_from(end).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::UVec2;
    use omuzaki_render_api::{AlphaEncoding, ColourSpace, PixelRect, Sampler, TextureDesc, TextureUsage};

    use super::*;
    use crate::gpu::{AdapterPreference, HeadlessOptions};
    use crate::readback::read_rgba8;
    use crate::resource::texture::GpuTexture;

    /// The pixels of mip `level`, which is `side` pixels across.
    fn read_level(gpu: &Gpu, texture: &GpuTexture, level: u32, side: u32) -> Vec<u8> {
        read_rgba8(gpu, &texture.sampled, level, UVec2::splat(side), AlphaEncoding::Straight).unwrap().rgba8
    }

    #[test]
    fn each_level_is_the_box_average_of_the_one_above() {
        let gpu = Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap();
        let mips = MipGenerator::new(&gpu.device);
        // Four 2x2 blocks of flat values: 0, 100, 200, 40 in red; the other channels carry other averages.
        let block = |v: u8| [v, 255 - v, v / 2, 255];
        let mut pixels = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                pixels.extend(block([0, 100, 200, 40][(y / 2) * 2 + x / 2]));
            }
        }
        let desc = TextureDesc {
            colour_space: ColourSpace::Linear,
            usage: TextureUsage::Static,
            sampler: Sampler::default(),
            ..TextureDesc::colour(UVec2::splat(4))
        };
        let texture = GpuTexture::create(&gpu, &mips, &desc, Some(&pixels));

        let first = read_level(&gpu, &texture, 1, 2);
        let expected: Vec<u8> = [0u8, 100, 200, 40].into_iter().flat_map(block).collect();
        assert!(first.iter().zip(&expected).all(|(a, b)| a.abs_diff(*b) <= 1), "{first:?} vs {expected:?}");

        let top = read_level(&gpu, &texture, 2, 1);
        let mean = 85u8; // (0 + 100 + 200 + 40) / 4
        assert!(top[0].abs_diff(mean) <= 1, "the 1x1 level averages everything: {top:?}");
        assert_eq!(top[3], 255, "alpha stays opaque");
    }

    /// The pixels of mip `level` of a texture `size` across at that level.
    fn read_size(gpu: &Gpu, texture: &GpuTexture, level: u32, size: UVec2) -> Vec<u8> {
        read_rgba8(gpu, &texture.sampled, level, size, AlphaEncoding::Straight).unwrap().rgba8
    }

    fn software() -> Gpu {
        Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap()
    }

    #[test]
    fn a_dynamic_textures_chain_can_be_rebuilt_after_a_paint() {
        let gpu = software();
        let mips = MipGenerator::new(&gpu.device);
        let desc = TextureDesc { colour_space: ColourSpace::Linear, ..TextureDesc::colour(UVec2::splat(2)).dynamic() };
        let texture = GpuTexture::create(&gpu, &mips, &desc, Some(&[[10, 10, 10, 255]; 4].concat()));
        assert_eq!(texture.mip_steps.len(), 1, "a dynamic texture keeps its blits");
        assert_eq!(read_level(&gpu, &texture, 1, 1)[0], 10);

        texture.write(&gpu, PixelRect::full(UVec2::splat(2)), &[[200, 200, 200, 255]; 4].concat());
        mips.refresh_now(&gpu, &texture);
        assert!(read_level(&gpu, &texture, 1, 1)[0].abs_diff(200) <= 1, "the new picture reached the small level");
    }

    #[test]
    fn an_odd_sized_chain_keeps_its_last_column() {
        let gpu = software();
        let mips = MipGenerator::new(&gpu.device);
        // 5x1: only the right-most texel is lit. Levels are 5, 2, 1 wide.
        let mut pixels = vec![0u8; 5 * 4];
        pixels[16..20].copy_from_slice(&[255, 255, 255, 255]);
        let desc = TextureDesc { colour_space: ColourSpace::Linear, ..TextureDesc::colour(UVec2::new(5, 1)) };
        let texture = GpuTexture::create(&gpu, &mips, &desc, Some(&pixels));
        let level_1 = read_size(&gpu, &texture, 1, UVec2::new(2, 1));
        assert_eq!(level_1[0], 0, "the left pixel covers texels 0 and 1 only");
        // Pixel 1 covers texels 2..5 (2.5 wide): the lit one is 1/2.5 of it.
        assert!(level_1[4].abs_diff(102) <= 2, "the right pixel keeps the edge texel: {level_1:?}");
        let level_2 = read_size(&gpu, &texture, 2, UVec2::new(1, 1));
        assert!(level_2[0] > 0, "the edge texel survives to the 1x1 level: {level_2:?}");
    }

    #[test]
    fn a_painter_texture_averages_in_linear_light_after_premultiplying() {
        let gpu = software();
        let mips = MipGenerator::new(&gpu.device);
        // Opaque black beside opaque white: the linear midpoint 0.5 encodes to sRGB 188, not 128.
        let pixels = [0, 0, 0, 255, 255, 255, 255, 255];
        let desc = TextureDesc::colour(UVec2::new(2, 1)).dynamic();
        let texture = GpuTexture::create(&gpu, &mips, &desc, Some(&pixels));
        let top = read_size(&gpu, &texture, 1, UVec2::new(1, 1));
        assert!(top[0].abs_diff(188) <= 1, "{top:?}");
        assert_eq!(top[3], 255);
    }

    #[test]
    fn a_painters_straight_colour_is_premultiplied_before_it_is_averaged() {
        let gpu = software();
        let mips = MipGenerator::new(&gpu.device);
        // Opaque green beside fully transparent black. Straight averaging would give half-dark green; premultiplied
        // averaging keeps the green colour and halves the alpha.
        let pixels = [0, 255, 0, 255, 0, 0, 0, 0];
        let desc = TextureDesc { colour_space: ColourSpace::Linear, ..TextureDesc::colour(UVec2::new(2, 1)).dynamic() };
        let texture = GpuTexture::create(&gpu, &mips, &desc, Some(&pixels));
        let top = read_size(&gpu, &texture, 1, UVec2::new(1, 1));
        // Stored premultiplied: green is alpha times 1, so both are about 128.
        assert!(top[1].abs_diff(128) <= 1 && top[3].abs_diff(128) <= 1, "{top:?}");
    }

    #[test]
    fn a_repaint_refreshes_only_the_changed_rectangle() {
        let gpu = software();
        let mips = MipGenerator::new(&gpu.device);
        let desc = TextureDesc { colour_space: ColourSpace::Linear, ..TextureDesc::colour(UVec2::splat(4)).dynamic() };
        let texture = GpuTexture::create(&gpu, &mips, &desc, Some(&[[100, 0, 0, 255]; 16].concat()));
        // Change the left half on the painter's side, but refresh only its top-left 2x2: the rest of the sampled picture
        // must stay as it was.
        texture.write(&gpu, PixelRect { x: 0, y: 0, width: 2, height: 4 }, &[[200, 0, 0, 255]; 8].concat());
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        mips.refresh(&mut encoder, &texture, PixelRect { x: 0, y: 0, width: 2, height: 2 });
        gpu.queue.submit([encoder.finish()]);
        let level_0 = read_size(&gpu, &texture, 0, UVec2::splat(4));
        assert!(level_0[0].abs_diff(200) <= 1, "the refreshed corner is new");
        assert!(level_0[(2 * 4) * 4].abs_diff(100) <= 1, "below it is untouched");
        let level_1 = read_size(&gpu, &texture, 1, UVec2::splat(2));
        assert!(level_1[0].abs_diff(200) <= 1, "the chain was rebuilt under the rectangle");
        assert!(level_1[2 * 4].abs_diff(100) <= 1, "and nowhere else");
    }

    #[test]
    fn rectangles_carry_down_a_chain_rounded_outward() {
        assert_eq!(carry((0, 5), 5, 2), (0, 2));
        assert_eq!(carry((4, 5), 5, 2), (1, 2), "the odd last texel feeds the last pixel");
        assert_eq!(carry((1, 2), 4, 2), (0, 1));
        assert_eq!(carry((3, 4), 8, 4), (1, 2));
    }
}
