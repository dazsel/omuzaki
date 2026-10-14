//! The `DepthOfField` pass: things away from the focal plane blur (LK-05).
//!
//! Three render passes on images half the target's size or smaller (shader notes in `shader/post_dof.wgsl`): a prepass
//! that reduces the scene colour and turns the depth into a signed circle of confusion (`CoC`), a pass that finds the widest
//! blurred foreground in each 8x8 block, and a gather in which each pixel averages a golden-angle spiral of neighbours
//! across a disc as wide as its own `CoC` (or as the foreground blurring over it). Neighbours in front of a pixel only count
//! if their own blur reaches it, which is what stops a sharp foreground (the character, outline included) from smearing
//! into the background behind it, while a blurred foreground still spreads over a sharp background. `Resolve` blends the
//! sharp scene with the gathered image by the blur width, so the in-focus part of the picture is never softened by the
//! half-size images.
//!
//! The model is a thin lens with a flat sharp zone (`DepthOfField::focus_range`): blur grows with the distance from the
//! zone divided by the distance from the camera, so a far wall blurs to at most `max_blur` and things very close blur
//! hard. `max_blur` is a fraction of the image height, so the look is the same at every `render_scale`.
//!
//! **Quality.** Every mode draws the same blur; what changes is how finely it is sampled (`Detail`). Live gathers about
//! 1.5 taps per pixel of blur radius (8 to 20) and compares one depth sample per place it looks; frame-exact offline
//! gathers 6 per pixel (up to 128) and compares four, which turns the live mode's fine noise into a smooth disc and keeps
//! thin silhouettes sharper. Cost follows the target's pixel count (not `render_scale`: see the `bloom` module) and the tap
//! count. A blurred foreground spreads at most 3 blocks (24 half-size pixels, 48 target pixels at 1080p) over a sharp
//! background; `MAX_BLUR` keeps every blur inside that.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{DepthOfField as Look, Pass, QualityMode, RenderError};

use super::bloom::reduction_taps;
use super::{FramePass, Outcome, PassCx};
use crate::gpu::SCENE_FORMAT;
use crate::shared::Shared;

/// The `CoC` image's format: one half-float per pixel, in pixels of the half-size images.
const COC_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
/// `max_blur` is pulled into `0..=MAX_BLUR` (a fourteenth of the image height, 77 pixels at 1080p): past that the gather's
/// spiral is too sparse to read.
const MAX_BLUR: f32 = 0.07;
/// The smallest side of the blocks the foreground search works in, in half-size pixels. A block is also never smaller than
/// the widest blur (`Request::tile`), so the nine blocks around a pixel always include a foreground that reaches it.
const MIN_TILE: u32 = 8;
/// A blur radius under this many half-size pixels (about one scene pixel) changes nothing visible: the pass is skipped.
const MIN_REACH: f32 = 0.5;
/// How many target sizes keep their images at once.
const CACHED_SIZES: usize = 3;

/// What a quality mode spends on the blur: gather taps (roughly `density` per pixel of blur radius, within
/// `MIN_TAPS..=cap`) and how many of a multisampled depth attachment's samples the prepass compares per place it looks.
/// The same blur at every setting; more taps and samples make it smoother and keep thin silhouettes sharper.
struct Detail {
    density: f32,
    cap: u32,
    depth_samples: u32,
}

impl Detail {
    const MIN_TAPS: u32 = 8;
    const LIVE: Self = Self { density: 1.5, cap: 20, depth_samples: 1 };
    const OFFLINE: Self = Self { density: 6.0, cap: 128, depth_samples: 4 };

    fn for_mode(mode: QualityMode) -> Self {
        match mode {
            QualityMode::Live => Self::LIVE,
            QualityMode::Offline => Self::OFFLINE,
        }
    }

    fn taps(&self, reach: f32) -> u32 {
        // `reach` is at most MAX_BLUR of an image height of a few thousand pixels.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let wanted = (reach * self.density).ceil() as u32;
        wanted.clamp(Self::MIN_TAPS, self.cap)
    }

    /// The depth samples to compare: all of the first few of a multisampled attachment, or the one of a plain one.
    fn depth_samples(&self, multisampled: bool, samples: u32) -> u32 {
        if multisampled { self.depth_samples.min(samples) } else { 1 }
    }
}

/// Mirrors `struct Params` in `shader/post_dof.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    depth: [f32; 4],
    focus: f32,
    half_range: f32,
    max_coc: f32,
    taps: u32,
    size: [f32; 2],
    full_size: [i32; 2],
    scale: [f32; 2],
    depth_samples: u32,
    colour_taps: u32,
    tile: i32,
    padding: [u32; 3],
}

/// What a frame asks of the pass, after sanitising: the look in the units the shader wants.
struct Request {
    focus: f32,
    half_range: f32,
    /// The largest blur radius in pixels of the half-size images.
    max_coc: f32,
}

impl Request {
    /// The side of the foreground search's blocks: the widest blur rounded up to a multiple of four, and at least
    /// `MIN_TILE`.
    fn tile(&self) -> u32 {
        // The widest blur is at most MAX_BLUR of an image height of a few thousand pixels.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let widest = self.max_coc.ceil() as u32;
        (widest.div_ceil(4) * 4).max(MIN_TILE)
    }
}

/// Half of `size`, rounded up (so the last odd row or column still has a pixel) and never below one.
fn half(size: UVec2) -> UVec2 {
    UVec2::new(size.x.div_ceil(2).max(1), size.y.div_ceil(2).max(1))
}

/// The look to draw: `None` when it is off, switched off by quality, or its blur would be under a scene pixel. Numbers the
/// contract does not validate are pulled in; a non-finite focus distance means there is nothing to focus on.
fn requested(cx: &PassCx<'_>) -> Option<Request> {
    let look: Look = cx.frame.scene.post.depth_of_field?;
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    // The image is a few thousand pixels tall at most, far below 2^24.
    #[allow(clippy::cast_precision_loss)]
    let height = cx.frame.target_size.y as f32;
    let max_coc = finite(look.max_blur).clamp(0.0, MAX_BLUR) * height * 0.5;
    let on = cx.frame.quality.post_effects && finite(look.focus_distance) > 0.0 && max_coc >= MIN_REACH;
    on.then(|| Request { focus: look.focus_distance, half_range: finite(look.focus_range).max(0.0) * 0.5, max_coc })
}

/// The depth-buffer-to-distance coefficients of a projection: with `d` the stored depth,
/// `distance = (d * m33 - m23) / (d * m32 - m22)`. It holds for perspective and orthographic alike.
fn depth_coefficients(projection: &omuzaki_core::glam::Mat4) -> [f32; 4] {
    [projection.w_axis.w, projection.w_axis.z, projection.z_axis.w, projection.z_axis.z]
}

/// The group-0 layout of the tiles, the gather and `Resolve`: an image, a bilinear sampler and a second image (the `CoC`, or the blur width). The same function
/// builds it for both, so the two are the same layout.
pub(super) fn layers_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
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
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("depth of field layers"),
        entries: &[
            texture(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            texture(2),
        ],
    })
}

/// The images for one target size.
struct Layers {
    size: UVec2,
    half: UVec2,
    /// The prepass's colour and `CoC`, half size.
    colour: wgpu::TextureView,
    coc: wgpu::TextureView,
    /// The foreground reach of each block of the request's tile size in half-size pixels, in the corner of an image sized
    /// for the smallest tile.
    tiles: wgpu::TextureView,
    /// The gather's result and the blur width it used, half size.
    blurred: wgpu::TextureView,
    reach: wgpu::TextureView,
    /// What the tiles and the gather read: the prepass's colour and `CoC`.
    gather_input: wgpu::BindGroup,
    /// What the gather also reads: the tiles.
    tiles_group: wgpu::BindGroup,
    /// What `Resolve` reads: the blurred image and the blur width.
    published: wgpu::BindGroup,
    params: wgpu::Buffer,
    params_group: wgpu::BindGroup,
}

impl Layers {
    fn new(dof: &DepthOfField, shared: &Shared, size: UVec2) -> Self {
        let device = &shared.gpu.device;
        let half = half(size);
        let image = |label: &str, format, size: UVec2| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let tile_size = UVec2::new(half.x.div_ceil(MIN_TILE), half.y.div_ceil(MIN_TILE));
        let colour = image("dof colour", SCENE_FORMAT, half);
        let coc = image("dof coc", COC_FORMAT, half);
        let tiles = image("dof tiles", COC_FORMAT, tile_size);
        let blurred = image("dof blurred", SCENE_FORMAT, half);
        let reach = image("dof reach", COC_FORMAT, half);
        let sampler = &shared.defaults.linear_clamp;
        let read = |first: &wgpu::TextureView, second: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dof layers"),
                layout: &dof.layers_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(first) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(second) },
                ],
            })
        };
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dof params"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dof params"),
            layout: &dof.params_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }],
        });
        let tiles_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dof tiles"),
            layout: &dof.tiles_layout,
            entries: &[wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tiles) }],
        });
        Self {
            size,
            half,
            gather_input: read(&colour, &coc),
            published: read(&blurred, &reach),
            tiles_group,
            colour,
            coc,
            tiles,
            blurred,
            reach,
            params,
            params_group,
        }
    }
}

/// The pipelines, compiled when first needed: one prepass per kind of depth attachment (multisampled or not), the block
/// search and the gather.
#[derive(Default)]
struct Pipelines {
    prepass_multisampled: Option<wgpu::RenderPipeline>,
    prepass_single: Option<wgpu::RenderPipeline>,
    tiles: Option<wgpu::RenderPipeline>,
    gather: Option<wgpu::RenderPipeline>,
}

/// See the module docs.
pub(crate) struct DepthOfField {
    layers_layout: wgpu::BindGroupLayout,
    params_layout: wgpu::BindGroupLayout,
    /// How the gather reads the tiles (binding 1, beside the prepass's depth at binding 0 of the same group index).
    tiles_layout: wgpu::BindGroupLayout,
    /// How the prepass reads depth: `[single sample, multisampled]`.
    depth_layouts: [wgpu::BindGroupLayout; 2],
    pipelines: Pipelines,
    layers: Vec<Layers>,
}

impl DepthOfField {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        let depth_layout = |multisampled| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("dof depth"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled,
                    },
                    count: None,
                }],
            })
        };
        Self {
            layers_layout: layers_layout(device),
            params_layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("dof params"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(size_of::<Params>() as u64),
                    },
                    count: None,
                }],
            }),
            tiles_layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("dof tiles"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            }),
            depth_layouts: [depth_layout(false), depth_layout(true)],
            pipelines: Pipelines::default(),
            layers: Vec::with_capacity(CACHED_SIZES),
        }
    }

    /// A pipeline for `entry` with the bind group layouts `groups`, drawing into `targets`.
    fn pipeline(
        device: &wgpu::Device,
        module: &wgpu::ShaderModule,
        entry: &str,
        groups: &[&wgpu::BindGroupLayout],
        targets: &[wgpu::TextureFormat],
    ) -> wgpu::RenderPipeline {
        let layouts: Vec<Option<&wgpu::BindGroupLayout>> = groups.iter().copied().map(Some).collect();
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(entry),
            bind_group_layouts: &layouts,
            immediate_size: 0,
        });
        let targets: Vec<Option<wgpu::ColorTargetState>> = targets.iter().map(|&format| Some(format.into())).collect();
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// Compiles whatever this frame needs that is not built yet.
    fn build_pipelines(&mut self, shared: &Shared, multisampled: bool) {
        let device = &shared.gpu.device;
        let module = |depth_type: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("depth of field"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../shader/post_dof.wgsl").replace("DEPTH_TYPE", depth_type).into(),
                ),
            })
        };
        let colour_input = &shared.layouts.colour_input;
        if self.pipelines.gather.is_none() {
            // Neither of these reads depth; the single-sample module is the cheaper one to compile them from.
            let module = module("texture_depth_2d");
            let layers = [&self.layers_layout, &self.params_layout];
            let tiles = Self::pipeline(device, &module, "fs_tiles", &layers, &[COC_FORMAT]);
            let gather = Self::pipeline(
                device,
                &module,
                "fs_gather",
                &[layers[0], layers[1], &self.tiles_layout],
                &[SCENE_FORMAT, COC_FORMAT],
            );
            (self.pipelines.tiles, self.pipelines.gather) = (Some(tiles), Some(gather));
        }
        let (slot, depth_type, layout) = if multisampled {
            (&self.pipelines.prepass_multisampled, "texture_depth_multisampled_2d", &self.depth_layouts[1])
        } else {
            (&self.pipelines.prepass_single, "texture_depth_2d", &self.depth_layouts[0])
        };
        if slot.is_none() {
            let prepass = Self::pipeline(
                device,
                &module(depth_type),
                "fs_prepass",
                &[colour_input, &self.params_layout, layout],
                &[SCENE_FORMAT, COC_FORMAT],
            );
            if multisampled {
                self.pipelines.prepass_multisampled = Some(prepass);
            } else {
                self.pipelines.prepass_single = Some(prepass);
            }
        }
    }

    /// The index in `self.layers` of the images for the working `size`, building them (and dropping the oldest set) if needed.
    fn layers_for(&mut self, shared: &Shared, size: UVec2) -> usize {
        if let Some(at) = self.layers.iter().position(|l| l.size == size) {
            return at;
        }
        if self.layers.len() == CACHED_SIZES {
            self.layers.remove(0);
        }
        let layers = Layers::new(self, shared, size);
        self.layers.push(layers);
        self.layers.len() - 1
    }
}

/// A colour attachment that is cleared, then drawn over completely.
fn clear_to(view: &wgpu::TextureView) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
    }
}

/// Opens a render pass over `attachments`.
fn begin<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    label: &str,
    attachments: &[Option<wgpu::RenderPassColorAttachment<'_>>],
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: attachments,
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// The frame's parameters for the shaders, for images `half` the target size.
fn frame_params(cx: &PassCx<'_>, request: &Request, half: UVec2) -> Params {
    let internal = cx.frame.internal_size;
    #[allow(clippy::cast_precision_loss)] // sizes are far below 2^24
    let (internal_f, half_f) = (internal.as_vec2(), half.as_vec2());
    let scale = internal_f / half_f;
    let detail = Detail::for_mode(cx.frame.quality.mode);
    Params {
        depth: depth_coefficients(&cx.frame.scene.camera.projection_matrix(internal_f.x / internal_f.y)),
        focus: request.focus,
        half_range: request.half_range,
        max_coc: request.max_coc,
        taps: detail.taps(request.max_coc),
        size: half_f.to_array(),
        full_size: internal.as_ivec2().to_array(),
        scale: scale.to_array(),
        depth_samples: detail.depth_samples(cx.frame.samples > 1, cx.frame.samples),
        colour_taps: reduction_taps(scale),
        tile: i32::try_from(request.tile()).unwrap_or(i32::MAX),
        padding: [0; 3],
    }
}

impl FramePass for DepthOfField {
    fn pass(&self) -> Pass {
        Pass::DepthOfField
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        let Some(request) = requested(cx) else { return Ok(Outcome::Skipped) };
        let shared = cx.shared;
        let multisampled = cx.frame.samples > 1;
        self.build_pipelines(shared, multisampled);
        let at = self.layers_for(shared, cx.frame.target_size);
        let prepass = if multisampled { &self.pipelines.prepass_multisampled } else { &self.pipelines.prepass_single };
        let (Some(prepass), Some(tiles), Some(gather), Some(layers)) =
            (prepass, &self.pipelines.tiles, &self.pipelines.gather, self.layers.get(at))
        else {
            return Err(RenderError::Backend("depth of field: the pipelines or images were not built".into()));
        };

        let params = frame_params(cx, &request, layers.half);
        shared.gpu.queue.write_buffer(&layers.params, 0, bytemuck::bytes_of(&params));

        let scene = cx.attachments.resolved_colour(cx.encoder).clone();
        let depth =
            cx.attachments.depth_input(&shared.gpu.device, &self.depth_layouts[usize::from(multisampled)]).clone();
        let mut pass = begin(cx.encoder, "dof prepass", &[Some(clear_to(&layers.colour)), Some(clear_to(&layers.coc))]);
        pass.set_pipeline(prepass);
        pass.set_bind_group(0, &scene, &[]);
        pass.set_bind_group(1, &layers.params_group, &[]);
        pass.set_bind_group(2, &depth, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        let mut pass = begin(cx.encoder, "dof tiles", &[Some(clear_to(&layers.tiles))]);
        // The blocks fill the corner of the image that the request's tile size needs.
        let blocks = layers.half.map(|side| side.div_ceil(request.tile())).as_vec2();
        pass.set_viewport(0.0, 0.0, blocks.x, blocks.y, 0.0, 1.0);
        pass.set_pipeline(tiles);
        pass.set_bind_group(0, &layers.gather_input, &[]);
        pass.set_bind_group(1, &layers.params_group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        let mut pass =
            begin(cx.encoder, "dof gather", &[Some(clear_to(&layers.blurred)), Some(clear_to(&layers.reach))]);
        pass.set_pipeline(gather);
        pass.set_bind_group(0, &layers.gather_input, &[]);
        pass.set_bind_group(1, &layers.params_group, &[]);
        pass.set_bind_group(2, &layers.tiles_group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);

        cx.attachments.post.depth_of_field = Some(layers.published.clone());
        cx.stats.draw_calls += 3;
        Ok(Outcome::Ran)
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec3;
    use omuzaki_render_api::Camera;

    use super::*;

    #[test]
    fn params_are_eighty_bytes() {
        assert_eq!(size_of::<Params>(), 80);
    }

    /// Distance from a depth value, as the shader computes it.
    fn distance_of(coefficients: [f32; 4], d: f32) -> f32 {
        let [m33, m23, m32, m22] = coefficients;
        (d * m33 - m23) / (d * m32 - m22)
    }

    #[test]
    fn depth_converts_back_to_distance_for_both_projections() {
        for camera in [
            Camera::default(),
            Camera::look_at(
                Vec3::new(0.0, 0.0, 5.0),
                Vec3::ZERO,
                Vec3::Y,
                omuzaki_render_api::Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
            ),
        ] {
            let projection = camera.projection_matrix(1.5);
            let coefficients = depth_coefficients(&projection);
            for distance in [0.5f32, 1.0, 2.5, 9.0] {
                let depth = projection.project_point3(Vec3::new(0.0, 0.0, -distance)).z;
                let back = distance_of(coefficients, depth);
                assert!((back - distance).abs() < 1e-3 * distance.max(1.0), "{distance} -> {depth} -> {back}");
            }
        }
    }

    #[test]
    fn tap_counts_scale_with_the_blur_and_with_quality() {
        assert_eq!(Detail::LIVE.taps(0.5), Detail::MIN_TAPS, "tiny blurs still use the minimum");
        assert_eq!(Detail::LIVE.taps(10.0), 15);
        assert_eq!(Detail::LIVE.taps(100.0), 20, "capped for live");
        assert_eq!(Detail::OFFLINE.taps(10.0), 60);
        assert_eq!(Detail::OFFLINE.taps(100.0), 128);
        assert!(Detail::for_mode(QualityMode::Offline).taps(10.0) > Detail::for_mode(QualityMode::Live).taps(10.0));
    }

    #[test]
    fn blocks_are_at_least_as_wide_as_the_widest_blur_and_never_under_the_minimum() {
        let tile = |max_coc| Request { focus: 1.0, half_range: 0.0, max_coc }.tile();
        assert_eq!((tile(0.5), tile(8.0), tile(8.1), tile(23.0), tile(37.8)), (8, 8, 12, 24, 40));
        assert!(tile(100.0) >= 100);
    }

    #[test]
    fn halves_round_up_so_an_odd_edge_keeps_its_pixel() {
        assert_eq!(half(UVec2::new(1921, 1080)), UVec2::new(961, 540));
        assert_eq!(half(UVec2::ONE), UVec2::ONE);
    }
}
