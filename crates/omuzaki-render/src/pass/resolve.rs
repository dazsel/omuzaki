//! The `Resolve` pass: the scene colour to the target's pixels.
//!
//! Reads the resolved HDR scene colour (linear, premultiplied) and writes the target in sRGB-encoded bytes: exposure,
//! ACES (three.js's variant, so the prototype's exposure values carry over) or a clamp, the sRGB curve, and the
//! target's alpha encoding. When the internal render is larger than the target (`render_scale` above 1) each output
//! pixel averages a grid of taps, tone-mapped before averaging so bright edges stay clean.
//!
//! It is also where the post effects meet the picture, because it already holds the scene colour and the target: the
//! glow `Bloom` published and the blurred image `DepthOfField` published are folded in per tap (so they cost one read
//! each, not an extra pass over an HDR image), and film grain, the colour grade and a one-level dither are applied to
//! the averaged *straight* display colour. Alpha is never touched by them, so a transparent pixel stays transparent and
//! the output is correct whether the target stores straight or premultiplied colour. `Quality::post_effects` off means
//! none of this runs and the pass is exactly exposure and tone mapping.
//!
//! The dither (triangular, one output level, only while an effect is on) is what keeps a bloom's long fall-off and a
//! graded gradient from banding in 8 bits; grain and dither are seeded from `FrameInfo::index` and the pixel alone, so a
//! frame re-renders to the same bytes.

use bytemuck::{Pod, Zeroable};
use omuzaki_render_api::{AlphaEncoding, ColourGrade, FilmGrain, Pass, RenderError, Tonemap};

use super::dof::layers_layout;
use super::{FramePass, Outcome, PassCx};
use crate::gpu::{OUTPUT_FORMAT, SCENE_FORMAT};
use crate::shared::Shared;

const FLAG_GLOW: u32 = 1;
const FLAG_DEPTH_OF_FIELD: u32 = 2;
const FLAG_GRADE: u32 = 4;
const FLAG_GRAIN: u32 = 8;
const FLAG_DITHER: u32 = 16;

/// Saturation and contrast are pulled into `0..=MAX_GRADE`: far past that the image is a poster, and the contrast
/// curve's power function loses meaning.
const MAX_GRADE: f32 = 8.0;
/// The film grain is one output pixel at 1080p and scales up with the target's height, so it looks the same at 4K.
const GRAIN_REFERENCE_HEIGHT: f32 = 1080.0;

/// Mirrors `struct Params` in `shader/post_resolve.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    exposure: f32,
    tonemap: u32,
    premultiplied: u32,
    taps: u32,
    ratio: [f32; 2],
    internal: [f32; 2],
    saturation: f32,
    contrast: f32,
    grain: f32,
    grain_cell: f32,
    seed: [u32; 2],
    flags: u32,
    dither: f32,
}

/// Taps per axis for an internal image `ratio` times the target: the footprint of one output pixel, rounded up.
/// Zero means the sizes match and one exact texel is read.
fn taps_for(ratio: f32) -> u32 {
    if (ratio - 1.0).abs() < 1e-6 {
        0
    } else {
        // render_scale is validated to 0.25..=4, so the ratio is at most 4.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let taps = ratio.ceil().max(1.0) as u32;
        taps
    }
}

/// The grade to apply, with its numbers pulled into range, or `None` when it would change nothing.
fn grade_to_apply(grade: Option<ColourGrade>) -> Option<ColourGrade> {
    let tame = |v: f32| if v.is_finite() { v.clamp(0.0, MAX_GRADE) } else { 1.0 };
    let grade = grade?;
    let (saturation, contrast) = (tame(grade.saturation), tame(grade.contrast));
    ((saturation - 1.0).abs() > f32::EPSILON || (contrast - 1.0).abs() > f32::EPSILON)
        .then_some(ColourGrade { saturation, contrast })
}

/// The grain amplitude to apply (`0..=1`), or `None` when there is none.
fn grain_to_apply(grain: Option<FilmGrain>) -> Option<f32> {
    let strength = grain?.strength;
    (strength.is_finite() && strength > 0.0).then_some(strength.min(1.0))
}

/// The size of one grain cell, in output pixels, for a target of `height` pixels.
fn grain_cell(height: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)] // heights are far below 2^24
    let cell = (height as f32 / GRAIN_REFERENCE_HEIGHT).round();
    cell.max(1.0)
}

/// A 1x1 transparent black image bound where an effect has nothing to publish, so one pipeline serves every frame.
fn empty(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("no post image"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// See the module docs.
pub(crate) struct Resolve {
    pipeline: wgpu::RenderPipeline,
    params: wgpu::Buffer,
    params_group: wgpu::BindGroup,
    /// Stand-ins for the glow and the depth-of-field layers when those effects did not run this frame.
    no_glow: wgpu::BindGroup,
    no_layers: wgpu::BindGroup,
}

impl Resolve {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("resolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shader/post_resolve.wgsl").into()),
        });
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("resolve params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layers = layers_layout(device);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("resolve"),
            bind_group_layouts: &[
                Some(&shared.layouts.colour_input),
                Some(&params_layout),
                Some(&shared.layouts.colour_input),
                Some(&layers),
            ],
            immediate_size: 0,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resolve params"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resolve params"),
            layout: &params_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("resolve"),
            layout: Some(&layout),
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
                    format: OUTPUT_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let sampler = &shared.defaults.linear_clamp;
        let (image, width) = (empty(device, SCENE_FORMAT), empty(device, wgpu::TextureFormat::R16Float));
        let texture =
            |binding, view| wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(view) };
        let filtered = wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) };
        let no_glow = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("no glow"),
            layout: &shared.layouts.colour_input,
            entries: &[texture(0, &image), filtered.clone()],
        });
        let no_layers = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("no depth of field"),
            layout: &layers,
            entries: &[texture(0, &image), filtered, texture(2, &width)],
        });
        Self { pipeline, params, params_group, no_glow, no_layers }
    }
}

/// What the shader needs to know about the frame: the output's encoding, the tap grid, which post images there are to fold
/// in and which display-space effects to apply to the average.
fn params_for(cx: &PassCx<'_>, glow: bool, depth_of_field: bool) -> Params {
    let post = &cx.frame.scene.post;
    let (internal, target) = (cx.frame.internal_size, cx.frame.target_size);
    #[allow(clippy::cast_precision_loss)] // sizes are far below 2^24
    let (internal_f, target_f) = (internal.as_vec2(), target.as_vec2());
    let ratio = internal_f / target_f;

    let effects = cx.frame.quality.post_effects;
    let grade = effects.then(|| grade_to_apply(post.grade)).flatten();
    let grain = effects.then(|| grain_to_apply(post.grain)).flatten();
    let any = glow || depth_of_field || grade.is_some() || grain.is_some();
    let flag = |on: bool, bit: u32| if on { bit } else { 0 };
    let frame = cx.frame.scene.frame.index;
    let grade_flag = flag(grade.is_some(), FLAG_GRADE);
    let neutral = ColourGrade { saturation: 1.0, contrast: 1.0 };
    let grade = grade.unwrap_or(neutral);
    Params {
        exposure: post.exposure,
        tonemap: match post.tonemap {
            Tonemap::Aces => 0,
            Tonemap::None => 1,
        },
        premultiplied: u32::from(cx.output_desc.alpha == AlphaEncoding::Premultiplied),
        taps: taps_for(ratio.x.max(ratio.y)),
        ratio: ratio.to_array(),
        internal: internal_f.to_array(),
        saturation: grade.saturation,
        contrast: grade.contrast,
        grain: grain.unwrap_or(0.0),
        grain_cell: grain_cell(target.y),
        // The frame index, split into words: the seed of grain and dither.
        #[allow(clippy::cast_possible_truncation)]
        seed: [frame as u32, (frame >> 32) as u32],
        flags: flag(glow, FLAG_GLOW)
            | flag(depth_of_field, FLAG_DEPTH_OF_FIELD)
            | grade_flag
            | flag(grain.is_some(), FLAG_GRAIN)
            | flag(any, FLAG_DITHER),
        dither: 1.0 / 255.0,
    }
}

impl FramePass for Resolve {
    fn pass(&self) -> Pass {
        Pass::Resolve
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        let glow = cx.attachments.post.glow.clone();
        let layers = cx.attachments.post.depth_of_field.clone();
        let params = params_for(cx, glow.is_some(), layers.is_some());
        cx.shared.gpu.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));

        let input = cx.attachments.resolved_colour(cx.encoder);
        let mut pass = cx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("resolve"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: cx.output,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, input, &[]);
        pass.set_bind_group(1, &self.params_group, &[]);
        pass.set_bind_group(2, glow.as_ref().unwrap_or(&self.no_glow), &[]);
        pass.set_bind_group(3, layers.as_ref().unwrap_or(&self.no_layers), &[]);
        pass.draw(0..3, 0..1);
        cx.stats.draw_calls += 1;
        Ok(Outcome::Ran)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_sixty_four_bytes() {
        assert_eq!(size_of::<Params>(), 64);
    }

    #[test]
    fn taps_cover_the_footprint_of_an_output_pixel() {
        assert_eq!(taps_for(1.0), 0, "same size: one exact texel");
        assert_eq!(taps_for(0.5), 1, "upscaling reads one bilinear tap");
        assert_eq!(taps_for(1.5), 2);
        assert_eq!(taps_for(2.0), 2);
        assert_eq!(taps_for(4.0), 4);
    }

    #[test]
    fn a_neutral_grade_and_no_grain_apply_nothing() {
        assert_eq!(grade_to_apply(None), None);
        assert_eq!(grade_to_apply(Some(ColourGrade { saturation: 1.0, contrast: 1.0 })), None);
        assert_eq!(grain_to_apply(None), None);
        assert_eq!(grain_to_apply(Some(FilmGrain { strength: 0.0 })), None);
    }

    #[test]
    fn grade_and_grain_numbers_are_pulled_into_range() {
        let grade = grade_to_apply(Some(ColourGrade { saturation: 99.0, contrast: -3.0 }));
        assert_eq!(grade, Some(ColourGrade { saturation: MAX_GRADE, contrast: 0.0 }));
        let grade = grade_to_apply(Some(ColourGrade { saturation: f32::NAN, contrast: 1.2 }));
        assert_eq!(grade, Some(ColourGrade { saturation: 1.0, contrast: 1.2 }), "a NaN is the neutral value");
        assert_eq!(grain_to_apply(Some(FilmGrain { strength: 7.0 })), Some(1.0));
        assert_eq!(grain_to_apply(Some(FilmGrain { strength: f32::NAN })), None);
    }

    #[test]
    fn grain_keeps_its_look_across_resolutions() {
        assert_eq!(grain_cell(720), 1.0);
        assert_eq!(grain_cell(1080), 1.0);
        assert_eq!(grain_cell(2160), 2.0, "4K grain cells are twice as wide");
    }
}
