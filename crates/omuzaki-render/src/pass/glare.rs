//! Lens glare (render-api [`Glare`](omuzaki_render_api::Glare)): streaks, a halo and ghosts from the scene's brightest
//! lights, drawn by the `Bloom` pass after its own glow and added to that glow (or published as the glow when bloom is
//! off), so `Resolve` folds both in with the one read it already does.
//!
//! On images half the *target's* size, like bloom's, so the look follows the output and not `render_scale`:
//! a high-pass at the glare's threshold, then for each streak axis (two streaks per axis) three passes of a
//! seven-tap line blur whose taps spread a quarter wider each pass (Kawase's light streaks), then one composite of the
//! axes, the halo and the ghosts. At most 1 + 3 × 4 + 1 = 14 small render passes. A frame with nothing above the
//! threshold draws a zero glare, so its pixels are exactly as without it.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::{UVec2, Vec2};
use omuzaki_render_api::{Glare as Look, MAX_STREAKS, RenderError};

use super::PassCx;
use super::bloom::reduction_taps;
use crate::gpu::SCENE_FORMAT;
use crate::shared::Shared;

/// Streak axes at most: two streaks each.
const AXES: usize = (MAX_STREAKS / 2) as usize;
/// Line-blur passes per axis.
const STEPS: usize = 3;
/// Each pass's taps are this much further apart than the last's.
const SPREAD: f32 = 4.0;
/// The first pass's step against the whole reach: three passes of three taps a side reach 3 × (1 + 4 + 16) = 63 steps.
const REACH_STEPS: f32 = 63.0;
/// The weight of each tap against the one nearer the light, in the first pass; pass `p` uses it to the power `4^p`, so
/// a streak fades the same way along its whole length.
const FALLOFF: f32 = 0.9;
/// Render passes: the high-pass, three per axis, the composite.
const STAGES: usize = 2 + STEPS * AXES;
const COMPOSITE: usize = STAGES - 1;
/// Dynamic-offset stride of the parameters, as bloom's.
const PARAMS_STRIDE: u64 = 256;
/// The glare's own scale: a streak spreads its light along its length, so its core is a few times dimmer than the light
/// it comes from; this brings `strength` 1 to a streak that reads clearly next to the light.
const GAIN: f32 = 6.0;
/// Where the composite reads each axis, and the bright image.
const AXIS_BINDINGS: [u32; AXES] = [0, 2, 3, 4];
const BRIGHT_BINDING: u32 = 5;

/// Mirrors `struct Params` in `shader/post_glare.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    inv_size: [f32; 2],
    direction: [f32; 2],
    footprint: [f32; 2],
    source_texel: [f32; 2],
    tint: [f32; 4],
    step: f32,
    falloff: f32,
    threshold: f32,
    strength: f32,
    halo: f32,
    ghosts: f32,
    aspect: f32,
    taps: u32,
    axes: u32,
    pad: [u32; 3],
}

/// The glare to draw: `None` when it is off, switched off by quality (glare rides bloom's switch), or empty. The
/// contract validates `Post`, but a non-finite number still counts as zero rather than poisoning the frame.
pub(super) fn requested(cx: &PassCx<'_>) -> Option<Look> {
    let look = cx.frame.scene.post.glare?;
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    let look = Look {
        strength: finite(look.strength).clamp(0.0, 4.0),
        threshold: finite(look.threshold).max(0.0),
        streaks: look.streaks.min(MAX_STREAKS) & !1,
        length: finite(look.length).clamp(0.01, 1.0),
        rotation_deg: finite(look.rotation_deg),
        halo: finite(look.halo).clamp(0.0, 1.0),
        ghosts: finite(look.ghosts).clamp(0.0, 1.0),
        ..look
    };
    (cx.frame.quality.post_effects.bloom && !look.is_empty()).then_some(look)
}

/// The size of the glare's images for a target size: half of it, at least one pixel.
fn glare_size(target: UVec2) -> UVec2 {
    UVec2::new(target.x.div_ceil(2).max(1), target.y.div_ceil(2).max(1))
}

/// The streak axes for `look`: unit directions in pixels (y down), `count` of them spread evenly over half a turn from
/// `rotation_deg` anticlockwise.
fn axes(look: &Look) -> impl Iterator<Item = Vec2> {
    let count = (look.streaks / 2) as usize;
    #[allow(clippy::cast_precision_loss)] // at most four
    let turn = std::f32::consts::PI / count.max(1) as f32;
    (0..count).map(move |k| {
        #[allow(clippy::cast_precision_loss)]
        let angle = look.rotation_deg.to_radians() + turn * k as f32;
        Vec2::new(angle.cos(), -angle.sin())
    })
}

/// Every stage's parameters for a frame of `look` at glare size `size` from a scene image of `source` pixels.
fn stage_params(look: &Look, size: UVec2, source: UVec2) -> [Params; STAGES] {
    let size_f = size.as_vec2();
    let inv_size = [1.0 / size_f.x, 1.0 / size_f.y];
    let base = Params { inv_size, ..Params::zeroed() };
    let mut out = [base; STAGES];
    let footprint = source.as_vec2() / size_f;
    out[0] = Params {
        footprint: footprint.to_array(),
        source_texel: [1.0 / source.as_vec2().x, 1.0 / source.as_vec2().y],
        threshold: look.threshold,
        taps: reduction_taps(footprint),
        ..base
    };
    let first_step = (look.length * size_f.y / REACH_STEPS).max(0.5);
    for (k, direction) in axes(look).enumerate() {
        for p in 0..STEPS {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // p < 3
            let power = SPREAD.powi(p as i32);
            out[1 + STEPS * k + p] = Params {
                direction: direction.to_array(),
                step: first_step * power,
                falloff: FALLOFF.powf(power),
                ..base
            };
        }
    }
    let tint = look.tint.to_linear();
    out[COMPOSITE] = Params {
        tint: [tint[0], tint[1], tint[2], 1.0],
        strength: look.strength * GAIN,
        halo: look.halo,
        ghosts: look.ghosts,
        aspect: size_f.x / size_f.y,
        axes: look.streaks / 2,
        ..base
    };
    out
}

/// One render pass: what it draws into and what it reads.
struct Stage {
    target: wgpu::TextureView,
    input: wgpu::BindGroup,
}

/// The images for one target size, built once and reused.
struct Chain {
    size: UVec2,
    glare_size: UVec2,
    /// The high-pass image; its input (the scene colour) is the frame's.
    bright: wgpu::TextureView,
    /// Per axis, its three passes: bright to the first step image, that to the second, the second to the axis image.
    streaks: Vec<Stage>,
    /// Reads the axes and the bright image.
    composite_input: wgpu::BindGroup,
    /// The glare's own glow image, drawn into when bloom did not run, and the bind group `Resolve` reads it through.
    own: wgpu::TextureView,
    own_glow: wgpu::BindGroup,
    params: wgpu::Buffer,
    params_group: wgpu::BindGroup,
}

impl Chain {
    fn new(glare: &GlareChain, shared: &Shared, target: UVec2) -> Self {
        let device = &shared.gpu.device;
        let sampler = &shared.defaults.linear_clamp;
        let size = glare_size(target);
        let image = |label: &str| {
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
                label: Some("glare input"),
                layout: &shared.layouts.colour_input,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
                ],
            })
        };
        let (bright, first, second, own) = (image("glare bright"), image("glare step 1"), image("glare step 2"), image("glare"));
        let axis_images: Vec<wgpu::TextureView> = (0..AXES).map(|_| image("glare axis")).collect();
        let (from_bright, from_first, from_second) = (read(&bright), read(&first), read(&second));
        let mut streaks = Vec::with_capacity(STEPS * AXES);
        for axis in &axis_images {
            streaks.push(Stage { target: first.clone(), input: from_bright.clone() });
            streaks.push(Stage { target: second.clone(), input: from_first.clone() });
            streaks.push(Stage { target: axis.clone(), input: from_second.clone() });
        }
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            wgpu::BindGroupEntry { binding: BRIGHT_BINDING, resource: wgpu::BindingResource::TextureView(&bright) },
        ];
        entries.extend(
            AXIS_BINDINGS
                .into_iter()
                .zip(&axis_images)
                .map(|(binding, view)| wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(view) }),
        );
        let composite_input = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glare composite input"),
            layout: &glare.composite_layout,
            entries: &entries,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glare params"),
            size: STAGES as u64 * PARAMS_STRIDE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glare params"),
            layout: &glare.params_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &params,
                    offset: 0,
                    size: wgpu::BufferSize::new(size_of::<Params>() as u64),
                }),
            }],
        });
        let own_glow = read(&own);
        Self { size: target, glare_size: size, bright, streaks, composite_input, own, own_glow, params, params_group }
    }
}

struct Pipelines {
    bright: wgpu::RenderPipeline,
    streak: wgpu::RenderPipeline,
    /// The composite into the glare's own image.
    composite: wgpu::RenderPipeline,
    /// The composite added onto bloom's glow.
    composite_add: wgpu::RenderPipeline,
}

/// The glare's shader, layouts, pipelines (compiled on first use) and images per target size.
pub(super) struct GlareChain {
    module: wgpu::ShaderModule,
    composite_layout: wgpu::BindGroupLayout,
    params_layout: wgpu::BindGroupLayout,
    single_input: wgpu::PipelineLayout,
    composite_input: wgpu::PipelineLayout,
    pipelines: Option<Pipelines>,
    chains: Vec<Chain>,
}

/// How many target sizes keep their images at once.
const CACHED_SIZES: usize = 3;

impl GlareChain {
    pub(super) fn new(shared: &Shared) -> Self {
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
        let mut entries: Vec<_> = AXIS_BINDINGS.into_iter().map(texture).collect();
        entries.push(texture(BRIGHT_BINDING));
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glare composite"),
            entries: &entries,
        });
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glare params"),
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
                label: Some("glare"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shader/post_glare.wgsl").into()),
            }),
            single_input: layout("glare", &shared.layouts.colour_input),
            composite_input: layout("glare composite", &composite_layout),
            composite_layout,
            params_layout,
            pipelines: None,
            chains: Vec::with_capacity(CACHED_SIZES),
        }
    }

    fn pipeline(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        entry: &str,
        blend: Option<wgpu::BlendState>,
    ) -> wgpu::RenderPipeline {
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
                    blend,
                    write_mask: wgpu::ColorWrites::COLOR,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

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

    /// Draws `look` from the frame's scene colour. With `onto` (bloom's glow image, which `Resolve` already reads), the
    /// glare is added to it; without, it is drawn into the glare's own image, which is published as the glow. Returns
    /// the render passes it recorded.
    ///
    /// # Errors
    /// [`RenderError::Backend`] if the pipelines or images were not built.
    pub(super) fn record(
        &mut self,
        cx: &mut PassCx<'_>,
        look: &Look,
        onto: Option<&wgpu::TextureView>,
    ) -> Result<u32, RenderError> {
        let shared = cx.shared;
        let device = &shared.gpu.device;
        if self.pipelines.is_none() {
            let add = wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::REPLACE,
            };
            self.pipelines = Some(Pipelines {
                bright: self.pipeline(device, &self.single_input, "fs_bright", None),
                streak: self.pipeline(device, &self.single_input, "fs_streak", None),
                composite: self.pipeline(device, &self.composite_input, "fs_composite", None),
                composite_add: self.pipeline(device, &self.composite_input, "fs_composite", Some(add)),
            });
        }
        let at = self.chain_for(shared, cx.frame.target_size);
        let (Some(pipelines), Some(chain)) = (&self.pipelines, self.chains.get(at)) else {
            return Err(RenderError::Backend("glare: the pipelines or images were not built".into()));
        };
        let params = stage_params(look, chain.glare_size, cx.frame.internal_size);
        for (i, stage) in params.iter().enumerate() {
            shared.gpu.queue.write_buffer(&chain.params, i as u64 * PARAMS_STRIDE, bytemuck::bytes_of(stage));
        }
        let scene = cx.attachments.resolved_colour(cx.encoder).clone();
        let mut passes = 0;
        draw(cx.encoder, &pipelines.bright, &chain.bright, &scene, chain, 0, true);
        passes += 1;
        let axes = (look.streaks / 2) as usize;
        for (i, stage) in chain.streaks.iter().take(STEPS * axes).enumerate() {
            draw(cx.encoder, &pipelines.streak, &stage.target, &stage.input, chain, 1 + i, true);
            passes += 1;
        }
        if let Some(glow) = onto {
            draw(cx.encoder, &pipelines.composite_add, glow, &chain.composite_input, chain, COMPOSITE, false);
        } else {
            draw(cx.encoder, &pipelines.composite, &chain.own, &chain.composite_input, chain, COMPOSITE, true);
            cx.attachments.post.glow = Some(chain.own_glow.clone());
        }
        Ok(passes + 1)
    }
}

/// One full-screen render pass: `pipeline` over `input` into `target` with the stage's parameters; `clear` starts from
/// black, else it draws over what is there.
fn draw(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    target: &wgpu::TextureView,
    input: &wgpu::BindGroup,
    chain: &Chain,
    stage: usize,
    clear: bool,
) {
    let load = if clear { wgpu::LoadOp::Clear(wgpu::Color::BLACK) } else { wgpu::LoadOp::Load };
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("glare"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, input, &[]);
    #[allow(clippy::cast_possible_truncation)] // fourteen stages
    pass.set_bind_group(1, &chain.params_group, &[(stage as u64 * PARAMS_STRIDE) as u32]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use omuzaki_render_api::Colour;

    use super::*;

    #[test]
    fn params_are_ninety_six_bytes() {
        assert_eq!(size_of::<Params>(), 96);
        assert!(size_of::<Params>() as u64 <= PARAMS_STRIDE);
    }

    #[test]
    fn streaks_spread_over_half_a_turn_from_the_rotation() {
        let star = Look { streaks: 4, rotation_deg: 0.0, ..Look::default() };
        let dirs: Vec<Vec2> = axes(&star).collect();
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].abs_diff_eq(Vec2::X, 1e-6) && dirs[1].abs_diff_eq(Vec2::new(0.0, -1.0), 1e-6), "{dirs:?}");
        assert_eq!(axes(&Look { streaks: 0, ..star }).count(), 0);
        assert_eq!(axes(&Look { streaks: 8, ..star }).count(), 4);
    }

    #[test]
    fn the_three_passes_reach_the_length_and_fade_alike() {
        let look = Look { length: 0.3, streaks: 2, tint: Colour::WHITE, ..Look::default() };
        let p = stage_params(&look, UVec2::new(960, 540), UVec2::new(1920, 1080));
        let steps: Vec<f32> = (1..=3).map(|i| p[i].step).collect();
        let reach = 3.0 * steps.iter().sum::<f32>();
        assert!((reach - 0.3 * 540.0).abs() < 1e-2, "three taps a side of each pass reach 30 % of the height: {reach}");
        assert!((p[2].falloff - FALLOFF.powi(4)).abs() < 1e-6, "a tap four steps out fades as four taps of the first");
        assert_eq!((p[0].taps, p[COMPOSITE].axes), (1, 1));
    }
}
