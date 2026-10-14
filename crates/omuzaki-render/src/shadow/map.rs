//! The shadow map of one render target and the settings its lookup reads.
//!
//! One depth texture, one uniform buffer and two bind groups over them: the *caster* group the `Shadows` pass draws
//! with (the matrix alone, because the map is its depth target) and the *receiver* group the lit passes sample (matrix,
//! lookup settings, the map and a comparison sampler). The map lives with the target's attachments, so a live frame
//! that skips the redraw still finds the map it drew a frame or two ago, and two targets never share one.
//!
//! The receiver group exists from the target's first frame, as a one-texel map with sampling switched off, so the lit
//! passes bind it unconditionally and a frame without shadows costs them one uniform read.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Quality, QualityMode, ShadowQuality};
use wgpu::util::DeviceExt;

use super::fit::Fit;
use crate::gpu::DEPTH_FORMAT;
use crate::shared::Shared;

/// How far a lookup point is pushed along its surface normal before it is compared, in metres: the prototype's
/// `normalBias` (three.js, in the prototype and the puppet stage). It removes acne on curved surfaces and costs the
/// shadow a little contact near its source, which is why it is kept at the prototype's value and no more.
pub(crate) const NORMAL_BIAS: f32 = 0.01;
/// The depth bias in metres: the prototype's `bias` of -0.0004 over a depth range of about six metres.
pub(crate) const DEPTH_BIAS: f32 = 0.0024;
/// Light directions closer than this (per component) are the same direction; a rig switch changes it by far more.
const SAME_DIRECTION: f32 = 1e-4;

/// The map's side in texels and the spacing of the lookup's 3 x 3 comparison taps in texels, for a quality. `None` for
/// [`ShadowQuality::Off`].
///
/// `Low` is the prototype's live look: its 1024 map over a window of about two metres, three.js's soft PCF. `High` has
/// four times the texels and spaces the taps twice as far, which keeps the same softness (the same width in metres) with
/// edges and contact points drawn at twice the resolution.
pub(crate) fn settings(quality: ShadowQuality) -> Option<(u32, f32)> {
    match quality {
        ShadowQuality::Off => None,
        ShadowQuality::Low => Some((1024, 1.0)),
        ShadowQuality::High => Some((2048, 2.0)),
    }
}

/// What `struct Shadow` in `shader/toon.wgsl` reads (and the first member, `view_proj`, is what `shadow.wgsl` reads).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowUniform {
    view_proj: [[f32; 4]; 4],
    /// x: the shadow-casting light's slot plus one (0: no shadow this frame), y: 1 / map size, z: depth bias in clip
    /// depth, w: normal bias in metres.
    params: [f32; 4],
    /// x: the spacing of the 3 x 3 comparison taps, in texels.
    kernel: [f32; 4],
}

/// Byte offset of `params` in the uniform: the part that switches sampling on and off.
const PARAMS_OFFSET: u64 = 64;

/// The shadow map of one target. See the module docs.
pub(crate) struct ShadowMap {
    size: u32,
    depth: wgpu::TextureView,
    uniform: wgpu::Buffer,
    caster_group: wgpu::BindGroup,
    receiver_group: wgpu::BindGroup,
    /// The light direction the map was last drawn for, if it holds a drawing at all.
    drawn_for: Option<Vec3>,
    /// Frames since the last drawing, counting the current one.
    since_drawn: u32,
    /// Whether the uniform currently tells the lookup to sample.
    sampling: bool,
}

impl ShadowMap {
    /// A map of `size` texels a side with sampling off.
    pub(crate) fn new(shared: &Shared, size: u32) -> Self {
        let device = &shared.gpu.device;
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("shadow map"),
                size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let off = ShadowUniform::zeroed();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shadow uniform"),
            contents: bytemuck::bytes_of(&off),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let caster_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow caster"),
            layout: &shared.layouts.shadow_caster,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        let receiver_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow receiver"),
            layout: &shared.layouts.shadow_receiver,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&depth) },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&shared.defaults.shadow_compare),
                },
            ],
        });
        Self { size, depth, uniform, caster_group, receiver_group, drawn_for: None, since_drawn: 0, sampling: false }
    }

    /// Replaces the map with one of `size` texels if it is not that size already (the quality changed). The new map is
    /// empty, so the next frame with a caster redraws it.
    pub(crate) fn resize(&mut self, shared: &Shared, size: u32) {
        if self.size != size {
            *self = Self::new(shared, size);
        }
    }

    /// The group the lit passes bind at group 2.
    pub(crate) fn receiver_group(&self) -> &wgpu::BindGroup {
        &self.receiver_group
    }

    /// Whether this frame must redraw the map, and counts the frame. Offline frames always do (frame-exact, no history);
    /// live frames redraw every `shadow_interval`-th frame, and at once when there is no drawing yet or the light turned.
    pub(crate) fn due(&mut self, quality: &Quality, to_light: Vec3) -> bool {
        self.since_drawn = self.since_drawn.saturating_add(1);
        let current = self.drawn_for.is_some_and(|drawn| drawn.abs_diff_eq(to_light, SAME_DIRECTION));
        quality.mode == QualityMode::Offline || !current || self.since_drawn >= u32::from(quality.shadow_interval)
    }

    /// Starts the render pass that draws casters into the map, clearing it to the far plane.
    pub(crate) fn begin_pass<'e>(&self, encoder: &'e mut wgpu::CommandEncoder) -> wgpu::RenderPass<'e> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow map"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// The caster group, for the pass that draws into the map.
    pub(crate) fn caster_group(&self) -> &wgpu::BindGroup {
        &self.caster_group
    }

    /// Publishes the matrix and settings of the map about to be drawn (shadows on, for the light in `light_slot`).
    pub(crate) fn publish(
        &mut self,
        queue: &wgpu::Queue,
        fit: &Fit,
        light_slot: usize,
        tap_spacing: f32,
        to_light: Vec3,
    ) {
        #[allow(clippy::cast_precision_loss)] // slots and sizes are tiny
        let uniform = ShadowUniform {
            view_proj: fit.view_proj.to_cols_array_2d(),
            params: [(light_slot + 1) as f32, 1.0 / self.size as f32, DEPTH_BIAS / fit.depth_range, NORMAL_BIAS],
            kernel: [tap_spacing, 0.0, 0.0, 0.0],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniform));
        self.drawn_for = Some(to_light);
        self.since_drawn = 0;
        self.sampling = true;
    }

    /// Switches sampling off (shadows off, or nothing left to cast), forgetting the drawing.
    pub(crate) fn disable(&mut self, queue: &wgpu::Queue) {
        if self.sampling {
            queue.write_buffer(&self.uniform, PARAMS_OFFSET, bytemuck::bytes_of(&[0.0f32; 4]));
            self.sampling = false;
        }
        self.drawn_for = None;
        self.since_drawn = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uniform_is_what_the_shader_declares() {
        assert_eq!(size_of::<ShadowUniform>(), 64 + 16 + 16);
        assert_eq!(std::mem::offset_of!(ShadowUniform, params) as u64, PARAMS_OFFSET);
    }

    #[test]
    fn quality_picks_size_and_softness_and_off_has_none() {
        assert_eq!(settings(ShadowQuality::Off), None);
        assert_eq!(settings(ShadowQuality::Low), Some((1024, 1.0)));
        assert_eq!(settings(ShadowQuality::High), Some((2048, 2.0)));
    }
}
