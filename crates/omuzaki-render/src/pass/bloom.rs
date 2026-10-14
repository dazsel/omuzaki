//! The `Bloom` pass: glow from the bright areas of the scene (LK-05), the prototype's `UnrealBloomPass` ported stage for
//! stage so its numbers carry over (strength 0.35, radius 0.5, threshold 0.9 give the prototype's look; the three.js
//! reference image in `tests/golden/` is the proof and `tests/post.rs` the check).
//!
//! Per frame, on the single-sample scene colour: a luminance high-pass into an image half the *target's* size, then five
//! mips (each half the one before) blurred horizontally and vertically by a gaussian of growing radius, then one
//! composite of the five weighted by `Bloom::radius`. That is 12 small render passes on images that shrink fast, so the
//! cost is mostly a fixed price per pass, not pixels. Live and offline quality draw the same bloom: there is no cheaper
//! bloom that keeps the prototype's look, and the live preset leaves post effects off.
//!
//! Bloom and depth of field work at the *target's* size whatever `render_scale` is. The scheduler lowers `render_scale` to
//! save time and must not change the author's look (D6), and a blur radius counted in pixels would; and supersampling would
//! otherwise multiply the cost of two effects that are low-frequency anyway. A larger scene image is reduced as the
//! high-pass reads it (a grid of bilinear taps over each pixel's footprint, so a thin bright line is not missed); a
//! smaller one is read with bilinear filtering.
//!
//! The pass does not touch the scene colour. It publishes the glow (`PostProducts::glow`) and `Resolve` adds it while it
//! already has the scene colour in hand, which saves a full-size read and write of an HDR image (about 0.3 ms at 1080p on
//! a 100 GB/s laptop GPU, more at 4K).
//!
//! Two details are kept from three.js on purpose. The threshold is compared with the linear luminance *before* exposure,
//! because the prototype's composer tone-maps after bloom. And `UnrealBloomPass` blends its result with `SRC_ALPHA`, so
//! the glow is also multiplied by its own alpha, which is `strength` times the sum of the mip weights (always 3): the
//! visible glow grows with the square of `strength`. `GAIN_PER_STRENGTH` is that factor.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{Bloom as Look, Pass, RenderError};

use super::{FramePass, Outcome, PassCx};
use crate::gpu::SCENE_FORMAT;
use crate::shared::Shared;

/// Blurred mips: the image divided by 2, 4, 8, 16 and 32.
const MIPS: usize = 5;
/// Gaussian taps per side, and its sigma in texels, for each mip (the prototype's `kernelSizeArray`).
const KERNELS: [f32; MIPS] = [3.0, 5.0, 7.0, 9.0, 11.0];
/// Render passes per frame: the high-pass, a horizontal and a vertical blur per mip, the composite.
const STAGES: usize = 2 + 2 * MIPS;
/// Where each stage's parameters sit in the one uniform buffer: dynamic offsets must be multiples of this.
const PARAMS_STRIDE: u64 = 256;
/// `strength` is pulled into `0..=MAX_STRENGTH`: at 8 the glow is already 192 times what the high-pass kept, near what a
/// half-float scene image can hold.
const MAX_STRENGTH: f32 = 8.0;
/// The mips' weights add up to this for every `Bloom::radius`.
const GAIN_PER_STRENGTH: f32 = 3.0;

/// Where the composite reads each blurred mip: the first at binding 0 and the sampler at 1, like every other stage.
const COMPOSITE_BINDINGS: [u32; MIPS] = [0, 2, 3, 4, 5];

/// Mirrors `struct Params` in `shader/post_bloom.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    direction: [f32; 2],
    inv_size: [f32; 2],
    radius: f32,
    strength: f32,
    gain: f32,
    threshold: f32,
    footprint: [f32; 2],
    source_texel: [f32; 2],
    taps: u32,
    padding: u32,
}

impl Params {
    /// A stage that blurs along `direction` at `size`, with a gaussian of `kernel` taps.
    fn blur(direction: [f32; 2], size: UVec2, kernel: f32) -> Self {
        Self { direction, radius: kernel, ..Self::stage(size) }
    }

    /// The high-pass into an image of `size` from a scene image of `source` pixels.
    fn bright(size: UVec2, source: UVec2, threshold: f32) -> Self {
        let footprint = source.as_vec2() / size.as_vec2();
        Self {
            threshold,
            footprint: footprint.to_array(),
            source_texel: inv_size(source),
            taps: reduction_taps(footprint),
            ..Self::stage(size)
        }
    }

    /// The composite into an image of `size`.
    fn composite(size: UVec2, look: &Look) -> Self {
        Self {
            radius: look.radius,
            strength: look.strength,
            gain: look.strength * GAIN_PER_STRENGTH,
            ..Self::stage(size)
        }
    }

    /// Everything zero but the size of the image the stage draws.
    fn stage(size: UVec2) -> Self {
        Self { inv_size: inv_size(size), ..Self::zeroed() }
    }
}

/// Bilinear taps per axis that reduce an image by `scale` (scene pixels per pixel of the result) without missing any of
/// it: one tap covers two pixels a side, so an exact 2:1 reduction is a single tap.
pub(super) fn reduction_taps(scale: omuzaki_core::glam::Vec2) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // scales are at most 8
    let taps = (scale.max_element() / 2.0).ceil().max(1.0) as u32;
    taps
}

/// Half of `size`, rounded to nearest and never below one pixel: the prototype's `Math.round(size / 2)`.
fn half(size: UVec2) -> UVec2 {
    UVec2::new(size.x.div_ceil(2).max(1), size.y.div_ceil(2).max(1))
}

/// The sizes of the five mips for a target size; the first is also the size of the high-pass image and the glow.
fn mip_sizes(target: UVec2) -> [UVec2; MIPS] {
    let mut sizes = [half(target); MIPS];
    for i in 1..MIPS {
        sizes[i] = half(sizes[i - 1]);
    }
    sizes
}

/// One over a size, as the shader wants it.
fn inv_size(size: UVec2) -> [f32; 2] {
    #[allow(clippy::cast_precision_loss)] // sizes are far below 2^24
    let size = size.as_vec2();
    [1.0 / size.x, 1.0 / size.y]
}

/// The bloom to draw: `None` when it is off, switched off by quality, or would add nothing. Out-of-range numbers (the
/// contract does not validate `Post`) are pulled in, and a non-finite one counts as zero rather than poisoning the frame.
fn requested(cx: &PassCx<'_>) -> Option<Look> {
    let look = cx.frame.scene.post.bloom?;
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    let strength = finite(look.strength).clamp(0.0, MAX_STRENGTH);
    (cx.frame.quality.post_effects && strength > 0.0).then(|| Look {
        strength,
        radius: finite(look.radius).clamp(0.0, 1.0),
        threshold: finite(look.threshold).max(0.0),
    })
}

/// One render pass of the chain: what it draws into and what it reads.
struct Stage {
    target: wgpu::TextureView,
    input: wgpu::BindGroup,
}

/// The images and bind groups of the chain for one target size. Built once per size, then reused every frame.
struct Chain {
    /// The target size this chain was built for.
    size: UVec2,
    /// The size of the first mip: of the high-pass image, of the glow and of the composite's target.
    glow_size: UVec2,
    /// The high-pass image. Its input is the scene colour, which only the frame knows.
    bright: wgpu::TextureView,
    /// Horizontal then vertical blur of each mip, in drawing order.
    blur: Vec<Stage>,
    /// Reads the five blurred mips and draws over the first mip's horizontal image, which is free by then.
    composite: Stage,
    /// The glow image (the composite's target) as the bind group `Resolve` reads.
    glow: wgpu::BindGroup,
    /// Every stage's parameters, `PARAMS_STRIDE` apart: the blurs' are written once, the others every frame.
    params: wgpu::Buffer,
    params_group: wgpu::BindGroup,
}

impl Chain {
    fn new(bloom: &Bloom, shared: &Shared, target: UVec2) -> Self {
        let device = &shared.gpu.device;
        let sampler = &shared.defaults.linear_clamp;
        let image = |label: &str, size: UVec2| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: SCENE_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let read = |view: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bloom input"),
                layout: &shared.layouts.colour_input,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
                ],
            })
        };

        let sizes = mip_sizes(target);
        let bright = image("bloom bright", sizes[0]);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bloom params"),
            size: STAGES as u64 * PARAMS_STRIDE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Each mip blurs the one before it (the first, the high-pass image): the horizontal blur reads that and the
        // vertical blur reads the horizontal one's result. The larger source is reduced by the bilinear fetch.
        let mut blur = Vec::with_capacity(2 * MIPS);
        let mut mips = Vec::with_capacity(MIPS);
        let mut previous = bright.clone();
        for (i, (&size, kernel)) in sizes.iter().zip(KERNELS).enumerate() {
            let (horizontal, vertical) = (image("bloom horizontal", size), image("bloom vertical", size));
            blur.push(Stage { target: horizontal.clone(), input: read(&previous) });
            blur.push(Stage { target: vertical.clone(), input: read(&horizontal) });
            for (axis, direction) in [[1.0, 0.0], [0.0, 1.0]].into_iter().enumerate() {
                let offset = (1 + 2 * i + axis) as u64 * PARAMS_STRIDE;
                shared.gpu.queue.write_buffer(
                    &params,
                    offset,
                    bytemuck::bytes_of(&Params::blur(direction, size, kernel)),
                );
            }
            previous = vertical.clone();
            mips.push((horizontal, vertical));
        }

        // The composite reads the five blurred (vertical) mips: the first at binding 0, the others after the sampler.
        let mut entries = vec![wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) }];
        entries.extend(COMPOSITE_BINDINGS.into_iter().zip(&mips).map(|(binding, (_, vertical))| {
            wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(vertical) }
        }));
        let composite = Stage {
            target: mips[0].0.clone(),
            input: device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bloom composite input"),
                layout: &bloom.composite_layout,
                entries: &entries,
            }),
        };
        // The first vertical blur reads exactly the image the composite then overwrites: that bind group is the glow.
        let glow = blur[1].input.clone();

        let params_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bloom params"),
            layout: &bloom.params_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &params,
                    offset: 0,
                    size: wgpu::BufferSize::new(size_of::<Params>() as u64),
                }),
            }],
        });
        Self { size: target, glow_size: sizes[0], bright, blur, composite, glow, params, params_group }
    }
}

/// The three pipelines, compiled on first use so a renderer that never turns bloom on never pays for them.
struct Pipelines {
    bright: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
}

/// See the module docs.
pub(crate) struct Bloom {
    module: wgpu::ShaderModule,
    /// Group 0 of the composite: the first mip, the sampler and the other four mips.
    composite_layout: wgpu::BindGroupLayout,
    /// Group 1 of every stage: its parameters, found by dynamic offset.
    params_layout: wgpu::BindGroupLayout,
    single_input: wgpu::PipelineLayout,
    composite_input: wgpu::PipelineLayout,
    pipelines: Option<Pipelines>,
    /// One chain per target size seen lately (a target per size is the usual case), the oldest dropped first.
    chains: Vec<Chain>,
}

/// How many target sizes keep their images at once.
const CACHED_SIZES: usize = 3;

impl Bloom {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let mut entries = vec![texture(0)];
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
        entries.extend(COMPOSITE_BINDINGS[1..].iter().copied().map(texture));
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bloom composite"),
            entries: &entries,
        });
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bloom params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(size_of::<Params>() as u64),
                },
                count: None,
            }],
        });
        let layout = |label, group0: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(group0), Some(&params_layout)],
                immediate_size: 0,
            })
        };
        Self {
            module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("bloom"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shader/post_bloom.wgsl").into()),
            }),
            single_input: layout("bloom", &shared.layouts.colour_input),
            composite_input: layout("bloom composite", &composite_layout),
            composite_layout,
            params_layout,
            pipelines: None,
            chains: Vec::with_capacity(CACHED_SIZES),
        }
    }

    fn pipeline(&self, device: &wgpu::Device, layout: &wgpu::PipelineLayout, entry: &str) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// The index in `self.chains` of the chain for `target`, building it (and dropping the oldest) if there is none.
    fn chain_for(&mut self, shared: &Shared, target: UVec2) -> usize {
        if let Some(at) = self.chains.iter().position(|c| c.size == target) {
            return at;
        }
        if self.chains.len() == CACHED_SIZES {
            self.chains.remove(0);
        }
        let chain = Chain::new(self, shared, target);
        self.chains.push(chain);
        self.chains.len() - 1
    }
}

/// One full-screen render pass: `pipeline` over `input` into `target`, with the stage's parameters at `offset`.
fn draw(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    target: &wgpu::TextureView,
    input: &wgpu::BindGroup,
    chain: &Chain,
    stage: usize,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("bloom"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, input, &[]);
    // Dynamic offsets are 32-bit by the API; the buffer is twelve stages long.
    #[allow(clippy::cast_possible_truncation)]
    pass.set_bind_group(1, &chain.params_group, &[(stage as u64 * PARAMS_STRIDE) as u32]);
    pass.draw(0..3, 0..1);
}

impl FramePass for Bloom {
    fn pass(&self) -> Pass {
        Pass::Bloom
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        let Some(look) = requested(cx) else { return Ok(Outcome::Skipped) };
        let shared = cx.shared;
        let device = &shared.gpu.device;
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines {
                bright: self.pipeline(device, &self.single_input, "fs_bright"),
                blur: self.pipeline(device, &self.single_input, "fs_blur"),
                composite: self.pipeline(device, &self.composite_input, "fs_composite"),
            });
        }
        let at = self.chain_for(shared, cx.frame.target_size);
        let (Some(pipelines), Some(chain)) = (&self.pipelines, self.chains.get(at)) else {
            return Err(RenderError::Backend("bloom: the pipelines or images were not built".into()));
        };

        let queue = &shared.gpu.queue;
        let high_pass = Params::bright(chain.glow_size, cx.frame.internal_size, look.threshold);
        queue.write_buffer(&chain.params, 0, bytemuck::bytes_of(&high_pass));
        let composite = Params::composite(chain.glow_size, &look);
        queue.write_buffer(&chain.params, (STAGES as u64 - 1) * PARAMS_STRIDE, bytemuck::bytes_of(&composite));

        let scene = cx.attachments.resolved_colour(cx.encoder).clone();
        draw(cx.encoder, &pipelines.bright, &chain.bright, &scene, chain, 0);
        for (i, stage) in chain.blur.iter().enumerate() {
            draw(cx.encoder, &pipelines.blur, &stage.target, &stage.input, chain, 1 + i);
        }
        draw(cx.encoder, &pipelines.composite, &chain.composite.target, &chain.composite.input, chain, STAGES - 1);

        cx.attachments.post.glow = Some(chain.glow.clone());
        cx.stats.draw_calls += u32::try_from(STAGES).unwrap_or(0);
        Ok(Outcome::Ran)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_fifty_six_bytes() {
        assert_eq!(size_of::<Params>(), 56);
        assert!(size_of::<Params>() as u64 <= PARAMS_STRIDE);
    }

    #[test]
    fn mips_halve_with_rounding_like_the_prototype() {
        // three.js: Math.round(size / 2) at every step, so 135 / 2 = 67.5 rounds up to 68.
        assert_eq!(
            mip_sizes(UVec2::new(1920, 1080)),
            [UVec2::new(960, 540), UVec2::new(480, 270), UVec2::new(240, 135), UVec2::new(120, 68), UVec2::new(60, 34)]
        );
        assert_eq!(mip_sizes(UVec2::new(3, 1)), [UVec2::new(2, 1), UVec2::ONE, UVec2::ONE, UVec2::ONE, UVec2::ONE]);
    }

    #[test]
    fn the_high_pass_takes_one_bilinear_tap_per_two_source_pixels_a_side() {
        let p = Params::bright(UVec2::new(960, 540), UVec2::new(1920, 1080), 0.9);
        assert_eq!((p.footprint, p.taps), ([2.0, 2.0], 1), "an exact 2:1 reduction is the prototype's single tap");
        let p = Params::bright(UVec2::new(960, 540), UVec2::new(2880, 1620), 0.9);
        assert_eq!((p.footprint, p.taps), ([3.0, 3.0], 2), "a 1.5x supersampled render takes a 2x2 grid");
    }

    #[test]
    fn the_mip_weights_always_add_up_to_the_gain_factor() {
        // The shader's factor(): mix(f, 1.2 - f, radius) over the five base weights.
        let base = [1.0f32, 0.8, 0.6, 0.4, 0.2];
        for radius in [0.0f32, 0.3, 0.5, 1.0] {
            let sum: f32 = base.iter().map(|f| f + (1.2 - f - f) * radius).sum();
            assert!((sum - GAIN_PER_STRENGTH).abs() < 1e-5, "radius {radius}: {sum}");
        }
    }
}
