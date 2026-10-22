//! The colour and depth a frame draws into, and the single-sample colour the post passes read.
//!
//! Scene passes draw HDR linear colour (premultiplied alpha) into a multisampled texture. The samples are resolved in
//! hardware, once, by whichever pass first needs the single-sample picture ([`Attachments::resolved_colour`]), so the
//! post passes and `Resolve` all read an ordinary texture. Each target owns one set; they are rebuilt when its internal
//! size or the sample count changes, never per frame. A supersampled frame (`frame/supersample.rs`) draws its geometry
//! in rounds and adds each round's resolved colour into one more texture, the average, which the post passes then read
//! in its place.

use omuzaki_core::glam::UVec2;

use super::supersample::Accumulate;
use crate::gpu::{DEPTH_FORMAT, SCENE_FORMAT};
use crate::shadow::map::ShadowMaps;
use crate::shared::Shared;

/// What the post passes leave for `Resolve` to composite. A pass sets its field while it records and `begin_frame`
/// clears both, so a pass that did not run (switched off, or nothing to do) leaves its field `None` and `Resolve` skips
/// it. The bind groups are cheap clones of ones the pass built once, so publishing allocates nothing.
#[derive(Default)]
pub(crate) struct PostProducts {
    /// Bloom's glow: linear, premultiplied, half the target's size, already scaled by the effect's strength.
    pub(crate) glow: Option<wgpu::BindGroup>,
    /// Depth of field's layers: the blurred image and how wide its blur is at each pixel, half the target's size.
    pub(crate) depth_of_field: Option<wgpu::BindGroup>,
}

/// Colour, depth and resolved colour for one target at one internal size and sample count.
pub(crate) struct Attachments {
    pub(crate) size: UVec2,
    pub(crate) samples: u32,
    /// The multisampled colour; `None` at one sample, where passes draw straight into `colour`.
    msaa: Option<wgpu::TextureView>,
    colour: wgpu::TextureView,
    /// The resolved colour as `Layouts::colour_input`, built once so post passes bind it without allocating.
    colour_input: wgpu::BindGroup,
    depth: wgpu::TextureView,
    /// The depth as a bind group, built the first time a pass asks ([`Attachments::depth_input`]).
    depth_input: Option<wgpu::BindGroup>,
    /// Products of the post passes for this frame's `Resolve`.
    pub(crate) post: PostProducts,
    /// The average of a supersampled frame's rounds and its bind group (`Layouts::colour_input`), made by the first frame
    /// that supersamples.
    average: Option<(wgpu::TextureView, wgpu::BindGroup)>,
    /// The first geometry pass of the round clears; later ones load.
    cleared: bool,
    resolved: bool,
    /// The rounds are added up: the post passes read the average.
    averaged: bool,
    pub(crate) bytes: u64,
    /// The target's shadow maps and their lookup settings: drawn by `Shadows`, bound by the lit passes at group 2. They
    /// are here, not in the pass, because a live frame that skips a redraw must still find the last maps of this target.
    pub(crate) shadow: ShadowMaps,
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    size: UVec2,
    samples: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// `view` as a post pass's input (`Layouts::colour_input`).
fn colour_input(shared: &Shared, view: &wgpu::TextureView, label: &str) -> wgpu::BindGroup {
    shared.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: &shared.layouts.colour_input,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&shared.defaults.linear_clamp),
            },
        ],
    })
}

impl Attachments {
    pub(crate) fn new(shared: &Shared, size: UVec2, samples: u32) -> Self {
        let device = &shared.gpu.device;
        let attach = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let readable = attach | wgpu::TextureUsages::TEXTURE_BINDING;
        let pixels = u64::from(size.x) * u64::from(size.y);
        let msaa = (samples > 1).then(|| texture(device, "scene colour (msaa)", size, samples, SCENE_FORMAT, attach));
        let colour = texture(device, "scene colour", size, 1, SCENE_FORMAT, readable);
        let colour_input = colour_input(shared, &colour, "scene colour");
        Self {
            size,
            samples,
            msaa,
            colour,
            colour_input,
            depth: texture(device, "scene depth", size, samples, DEPTH_FORMAT, readable),
            depth_input: None,
            post: PostProducts::default(),
            average: None,
            cleared: false,
            resolved: false,
            averaged: false,
            shadow: ShadowMaps::new(shared, 1, 1),
            // 8 bytes of colour per sample (plus the resolved copy when multisampled), 4 of depth.
            bytes: pixels * (8 + if samples > 1 { 8 * u64::from(samples) } else { 0 } + 4 * u64::from(samples)),
        }
    }

    /// Whether these attachments fit a frame of `size` at `samples`.
    pub(crate) fn fits(&self, size: UVec2, samples: u32) -> bool {
        self.size == size && self.samples == samples
    }

    /// Forgets the previous frame's clear, resolve and average.
    pub(crate) fn begin_frame(&mut self) {
        self.begin_round();
        self.averaged = false;
        self.post = PostProducts::default();
    }

    /// Starts another round of a supersampled frame: its first geometry pass clears again.
    pub(crate) fn begin_round(&mut self) {
        self.cleared = false;
        self.resolved = false;
    }

    /// Adds the round just drawn into the average, weighted by `1 / rounds`; after the last round the post passes read
    /// the average instead of the scene colour. Makes the average's texture the first time.
    pub(crate) fn accumulate(
        &mut self,
        shared: &Shared,
        encoder: &mut wgpu::CommandEncoder,
        accumulate: &Accumulate,
        round: u32,
        rounds: u32,
    ) {
        if self.average.is_none() {
            let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
            let view = texture(&shared.gpu.device, "scene colour (average)", self.size, 1, SCENE_FORMAT, usage);
            let input = colour_input(shared, &view, "scene colour (average)");
            self.bytes += 8 * u64::from(self.size.x) * u64::from(self.size.y);
            self.average = Some((view, input));
        }
        let input = self.resolved_colour(encoder).clone();
        if let Some((sum, _)) = &self.average {
            accumulate.record(encoder, &input, sum, round, rounds);
        }
        self.averaged = round + 1 >= rounds;
    }

    /// Starts a render pass that draws geometry into the scene colour and depth. The first one in a frame clears to
    /// `background` (premultiplied linear) and depth 1; the rest load what is there. Whichever pass opens the frame
    /// therefore sets the background, with no pass needing to know it is first.
    pub(crate) fn geometry_pass<'e>(
        &mut self,
        encoder: &'e mut wgpu::CommandEncoder,
        label: &str,
        background: wgpu::Color,
    ) -> wgpu::RenderPass<'e> {
        let (colour_load, depth_load) = if self.cleared {
            (wgpu::LoadOp::Load, wgpu::LoadOp::Load)
        } else {
            (wgpu::LoadOp::Clear(background), wgpu::LoadOp::Clear(1.0))
        };
        self.cleared = true;
        self.resolved = false;
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: self.msaa.as_ref().unwrap_or(&self.colour),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: colour_load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth,
                depth_ops: Some(wgpu::Operations { load: depth_load, store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// The depth attachment as a one-binding bind group of `layout` (a depth texture, multisampled exactly when these
    /// attachments are), built on first use and kept until the attachments are rebuilt. The layout is the caller's, so
    /// the pass that reads depth owns how it reads it; it must be the same layout on every call.
    pub(crate) fn depth_input(&mut self, device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> &wgpu::BindGroup {
        let depth = &self.depth;
        self.depth_input.get_or_insert_with(|| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("scene depth"),
                layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(depth) }],
            })
        })
    }

    /// The single-sample scene colour as a bind group for a post pass (`Layouts::colour_input`), resolving the
    /// multisampled colour first if geometry has been drawn since the last resolve. Post passes and `Resolve` call
    /// this; the first caller pays for the resolve. Once a supersampled frame's rounds are added up, it is the average.
    pub(crate) fn resolved_colour(&mut self, encoder: &mut wgpu::CommandEncoder) -> &wgpu::BindGroup {
        if self.averaged
            && let Some((_, average)) = &self.average
        {
            return average;
        }
        if let (Some(msaa), false) = (&self.msaa, self.resolved) {
            // An otherwise empty pass: loading the samples and storing nothing but the resolve is how wgpu resolves.
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("resolve samples"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa,
                    depth_slice: None,
                    resolve_target: Some(&self.colour),
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Discard },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.resolved = true;
        &self.colour_input
    }
}
