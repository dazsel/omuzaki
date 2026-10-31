//! The shadow maps of one render target and the settings their lookup reads.
//!
//! One layered depth texture (a layer per shadow-casting directional light, up to
//! [`MAX_SHADOW_MAPS`](omuzaki_render_api::limits::MAX_SHADOW_MAPS)), two uniform buffers and two bind groups: the
//! *caster* group the `Shadows` pass draws with (one map's matrix, picked by a dynamic offset, because that map's layer
//! is its depth target) and the *receiver* group the lit passes sample (every map's matrix and lookup settings, the
//! layers and a comparison sampler). The maps live with the target's attachments, so a live frame that skips a redraw
//! still finds the maps it drew a frame or two ago, and two targets never share one.
//!
//! Each map keeps its own state: the light it belongs to, the direction it was drawn for and the frames since, so the
//! live interval and a turned light redraw it alone.
//!
//! The receiver group exists from the target's first frame, as one one-texel layer with sampling switched off, so the
//! lit passes bind it unconditionally and a frame without shadows costs them one uniform read per directional light.
//!
//! **The hair shadow** (#2528) lives here too: the hair alone from the camera, at the frame's internal size, as each
//! texel's distance from the eye, which the `Shadows` pass redraws every frame there is hair to cast it, and whose
//! lookup settings follow the maps in the receiver uniform. A surface that receives it (`hair_shadow` in `shader/toon.wgsl`) marches toward each
//! shadow-casting directional light through that depth: the fringe hangs a centimetre or two off the forehead, inside
//! the maps' normal bias, so the maps cannot draw its shadow on the face.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::Vec3;
use omuzaki_render_api::limits::MAX_SHADOW_MAPS;
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
/// How far toward the light the hair shadow's march goes, in metres: past the deepest fringe's gap at a low light.
const HAIR_REACH: f32 = 0.05;
/// How far behind a hair surface a point still lies under the hair, in metres: a fringe's thickness and a little more,
/// short of the head behind it.
const HAIR_THICKNESS: f32 = 0.03;
/// How much of the light the hair takes away (1: the surface's full shade tone).
const HAIR_STRENGTH: f32 = 1.0;

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

/// One map's entry in `struct Shadows` of `shader/toon.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowUniform {
    view_proj: [[f32; 4]; 4],
    /// x: the shadow-casting light's slot plus one (0: this map is off), y: 1 / map size, z: depth bias in clip depth,
    /// w: normal bias in metres.
    params: [f32; 4],
    /// x: the spacing of the 3 x 3 comparison taps, in texels; y: the soft disk's radius in texels (0: the 3 x 3 grid).
    kernel: [f32; 4],
}

/// `struct Shadows` of `shader/toon.wgsl`: every map's entry, then the hair shadow's settings.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ReceiverUniform {
    maps: [ShadowUniform; MAX_SHADOW_MAPS],
    /// x: 1 when the hair depth holds this frame's hair, y: the march's reach and z the hair's thickness in metres,
    /// w: the strength.
    hair: [f32; 4],
}

/// Byte offset of `params` in an entry: the part that switches a map on and off.
const PARAMS_OFFSET: u64 = 64;
/// Byte offset of the soft disk's radius (`kernel.y`) in an entry.
const SOFTNESS_OFFSET: u64 = 84;
/// The soft disk's radius at a scene's `shadow_softness` of 1, in world metres: a penumbra about 8 cm wide. The maps are
/// fitted to their casters, so a texel's size in the world changes from frame to frame; the radius is set in metres.
const SOFT_METRES: f32 = 0.04;
/// The most texels the soft disk spans, so that a map fitted tightly to a small caster keeps its 16 taps close enough
/// to blend.
const SOFT_MAX_TEXELS: f32 = 160.0;
/// Bytes between two entries of the receiver uniform (a WGSL uniform array's stride).
const ENTRY_BYTES: u64 = size_of::<ShadowUniform>() as u64;
/// Byte offset of the hair shadow's settings in the receiver uniform.
const HAIR_OFFSET: u64 = std::mem::offset_of!(ReceiverUniform, hair) as u64;
/// Bytes between two matrices of the caster uniform: the dynamic offsets must be multiples of the device's
/// `min_uniform_buffer_offset_alignment`, which is at most 256.
const CASTER_STRIDE: u64 = 256;
/// The size of one caster binding: the matrix `shadow.wgsl` reads.
const CASTER_BYTES: u64 = 64;

/// What one map holds and for which light.
#[derive(Copy, Clone, Default)]
struct MapState {
    /// The light slot and direction the map was last drawn for, if it holds a drawing at all.
    drawn_for: Option<(usize, Vec3)>,
    /// Frames since the last drawing, counting the current one.
    since_drawn: u32,
    /// Whether the uniform currently tells the lookup to sample this map.
    sampling: bool,
    /// The soft disk's radius the uniform holds for this map, in texels.
    softness: f32,
    /// World metres a texel of this map's drawing covers.
    texel: f32,
}

/// The shadow maps of one target. See the module docs.
pub(crate) struct ShadowMaps {
    size: u32,
    /// One view per layer, each the depth target of its map's drawing.
    layers: Vec<wgpu::TextureView>,
    uniform: wgpu::Buffer,
    casters: wgpu::Buffer,
    caster_group: wgpu::BindGroup,
    receiver_group: wgpu::BindGroup,
    maps: [MapState; MAX_SHADOW_MAPS],
    /// Every layer, as the receiver group binds them (kept to rebuild the group when the hair texture changes size).
    all: wgpu::TextureView,
    /// The hair textures' size, the distance the lit passes read and the depth that keeps the nearest hair.
    hair_size: (u32, u32),
    hair_distance: wgpu::TextureView,
    hair_depth: wgpu::TextureView,
    /// Whether the uniform tells the lookup to read the hair distance.
    hair_on: bool,
}

/// The format of the hair distance: one float a texel, the distance from the eye along the view axis. A colour
/// texture, not the depth itself, because the lit shaders read it with a plain load (a depth texture loaded beside the
/// compared shadow maps crashes the software rasteriser).
pub(crate) const HAIR_DISTANCE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

/// One `width` x `height` texture of the hair pass: the distance (`format` [`HAIR_DISTANCE_FORMAT`], read by the lit
/// passes) or its depth.
fn hair_texture(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat) -> wgpu::TextureView {
    let sampled = if format == HAIR_DISTANCE_FORMAT {
        wgpu::TextureUsages::TEXTURE_BINDING
    } else {
        wgpu::TextureUsages::empty()
    };
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("hair shadow"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | sampled,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

fn receiver_group(
    shared: &Shared,
    uniform: &wgpu::Buffer,
    all: &wgpu::TextureView,
    hair_distance: &wgpu::TextureView,
) -> wgpu::BindGroup {
    shared.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("shadow receiver"),
        layout: &shared.layouts.shadow_receiver,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(all) },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&shared.defaults.shadow_compare),
            },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(hair_distance) },
        ],
    })
}

impl ShadowMaps {
    /// `layers` maps of `size` texels a side (at least one layer), with sampling off.
    pub(crate) fn new(shared: &Shared, size: u32, layers: usize) -> Self {
        let device = &shared.gpu.device;
        #[allow(clippy::cast_possible_truncation)] // at most MAX_SHADOW_MAPS
        let count = layers.clamp(1, MAX_SHADOW_MAPS) as u32;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow maps"),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: count },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let all = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layers = (0..count)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("shadow map layer"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shadow uniform"),
            contents: bytemuck::bytes_of(&ReceiverUniform::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let casters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shadow casters"),
            size: CASTER_STRIDE * MAX_SHADOW_MAPS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let caster_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow caster"),
            layout: &shared.layouts.shadow_caster,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &casters,
                    offset: 0,
                    size: wgpu::BufferSize::new(CASTER_BYTES),
                }),
            }],
        });
        let hair_distance = hair_texture(device, 1, 1, HAIR_DISTANCE_FORMAT);
        let hair_depth = hair_texture(device, 1, 1, DEPTH_FORMAT);
        let receiver_group = receiver_group(shared, &uniform, &all, &hair_distance);
        Self {
            size,
            layers,
            uniform,
            casters,
            caster_group,
            receiver_group,
            maps: [MapState::default(); MAX_SHADOW_MAPS],
            all,
            hair_size: (1, 1),
            hair_distance,
            hair_depth,
            hair_on: false,
        }
    }

    /// Makes room for `count` maps of `size` texels, replacing the texture if it is another size or has too few layers
    /// (the quality or the rig changed). New maps are empty, so the next frame redraws them.
    pub(crate) fn resize(&mut self, shared: &Shared, size: u32, count: usize) {
        if self.size != size || self.layers.len() < count {
            *self = Self::new(shared, size, count);
        }
    }

    /// The group the lit passes bind at group 2.
    pub(crate) fn receiver_group(&self) -> &wgpu::BindGroup {
        &self.receiver_group
    }

    /// Whether map `index` must be redrawn this frame for the light in `slot` toward `to_light`, and counts the frame.
    /// Offline frames always redraw (frame-exact, no history); live frames redraw every `shadow_interval`-th frame, and
    /// at once when the map holds no drawing yet or one for another light or direction.
    pub(crate) fn due(&mut self, index: usize, quality: &Quality, slot: usize, to_light: Vec3) -> bool {
        let Some(map) = self.maps.get_mut(index) else { return false };
        map.since_drawn = map.since_drawn.saturating_add(1);
        let current = map
            .drawn_for
            .is_some_and(|(drawn_slot, drawn)| drawn_slot == slot && drawn.abs_diff_eq(to_light, SAME_DIRECTION));
        quality.mode == QualityMode::Offline || !current || map.since_drawn >= u32::from(quality.shadow_interval)
    }

    /// Starts the render pass that draws casters into map `index`, clearing it to the far plane. `None` past the
    /// layers [`resize`](Self::resize) made room for.
    pub(crate) fn begin_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        index: usize,
    ) -> Option<wgpu::RenderPass<'e>> {
        let view = self.layers.get(index)?;
        Some(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shadow map"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }))
    }

    /// The caster group and the dynamic offset that picks map `index`'s matrix, for the pass that draws into it.
    pub(crate) fn caster_group(&self, index: usize) -> (&wgpu::BindGroup, u32) {
        #[allow(clippy::cast_possible_truncation)] // at most MAX_SHADOW_MAPS * 256
        (&self.caster_group, (index as u64 * CASTER_STRIDE) as u32)
    }

    /// Publishes the matrix and settings of map `index` about to be drawn for the light in `light_slot` (that map on).
    pub(crate) fn publish(
        &mut self,
        queue: &wgpu::Queue,
        index: usize,
        fit: &Fit,
        light_slot: usize,
        tap_spacing: f32,
        to_light: Vec3,
    ) {
        let Some(map) = self.maps.get_mut(index) else { return };
        // The soft radius stays what it was; `set_softness` brings it to this frame's.
        let radius = map.softness;
        #[allow(clippy::cast_precision_loss)] // slots and sizes are tiny
        let uniform = ShadowUniform {
            view_proj: fit.view_proj.to_cols_array_2d(),
            params: [(light_slot + 1) as f32, 1.0 / self.size as f32, DEPTH_BIAS / fit.depth_range, NORMAL_BIAS],
            kernel: [tap_spacing, radius, 0.0, 0.0],
        };
        queue.write_buffer(&self.uniform, index as u64 * ENTRY_BYTES, bytemuck::bytes_of(&uniform));
        queue.write_buffer(&self.casters, index as u64 * CASTER_STRIDE, bytemuck::bytes_of(&uniform.view_proj));
        *map = MapState {
            drawn_for: Some((light_slot, to_light)),
            since_drawn: 0,
            sampling: true,
            softness: uniform.kernel[1],
            texel: fit.texel,
        };
    }

    /// Starts the render pass that draws the hair's distance, `width` x `height` texels (the frame's internal size; a new
    /// size replaces the textures and the receiver group), clearing the distance to 0 (no hair) and the depth to the far
    /// plane.
    pub(crate) fn begin_hair_pass<'e>(
        &mut self,
        shared: &Shared,
        encoder: &'e mut wgpu::CommandEncoder,
        width: u32,
        height: u32,
    ) -> wgpu::RenderPass<'e> {
        let size = (width.max(1), height.max(1));
        if self.hair_size != size {
            let device = &shared.gpu.device;
            self.hair_distance = hair_texture(device, size.0, size.1, HAIR_DISTANCE_FORMAT);
            self.hair_depth = hair_texture(device, size.0, size.1, DEPTH_FORMAT);
            self.receiver_group = receiver_group(shared, &self.uniform, &self.all, &self.hair_distance);
            self.hair_size = size;
        }
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("hair shadow"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.hair_distance,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.hair_depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// Tells the lookup whether the hair distance holds this frame's hair.
    pub(crate) fn set_hair(&mut self, queue: &wgpu::Queue, on: bool) {
        if self.hair_on != on {
            let hair = if on { [1.0, HAIR_REACH, HAIR_THICKNESS, HAIR_STRENGTH] } else { [0.0; 4] };
            queue.write_buffer(&self.uniform, HAIR_OFFSET, bytemuck::bytes_of(&hair));
            self.hair_on = on;
        }
    }

    /// Brings every sampled map's soft radius to `softness` (a scene's `shadow_softness`), after this frame's maps are
    /// published.
    pub(crate) fn set_softness(&mut self, queue: &wgpu::Queue, softness: f32) {
        for (index, map) in self.maps.iter_mut().enumerate() {
            let radius = if map.texel > 0.0 {
                (softness.clamp(0.0, 1.0) * SOFT_METRES / map.texel).min(SOFT_MAX_TEXELS)
            } else {
                0.0
            };
            if map.sampling && map.softness.to_bits() != radius.to_bits() {
                queue.write_buffer(
                    &self.uniform,
                    index as u64 * ENTRY_BYTES + SOFTNESS_OFFSET,
                    bytemuck::bytes_of(&radius),
                );
                map.softness = radius;
            }
        }
    }

    /// Switches map `index` off (its light no longer casts, or nothing is left to cast), forgetting its drawing.
    pub(crate) fn disable(&mut self, queue: &wgpu::Queue, index: usize) {
        let Some(map) = self.maps.get_mut(index) else { return };
        if map.sampling {
            queue.write_buffer(
                &self.uniform,
                index as u64 * ENTRY_BYTES + PARAMS_OFFSET,
                bytemuck::bytes_of(&[0.0f32; 4]),
            );
        }
        *map = MapState::default();
    }

    /// Switches off every map from `first` on.
    pub(crate) fn disable_from(&mut self, queue: &wgpu::Queue, first: usize) {
        for index in first..MAX_SHADOW_MAPS {
            self.disable(queue, index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uniform_is_what_the_shader_declares() {
        assert_eq!(size_of::<ShadowUniform>(), 64 + 16 + 16);
        assert_eq!(SOFTNESS_OFFSET, std::mem::offset_of!(ShadowUniform, kernel) as u64 + 4, "the radius is kernel.y");
        assert_eq!(ENTRY_BYTES % 16, 0, "a uniform array's stride is a multiple of 16");
        assert_eq!(std::mem::offset_of!(ShadowUniform, params) as u64, PARAMS_OFFSET);
        assert_eq!(HAIR_OFFSET, ENTRY_BYTES * MAX_SHADOW_MAPS as u64, "the hair's settings follow the maps");
        assert_eq!(size_of::<ReceiverUniform>() as u64, HAIR_OFFSET + 16);
        const { assert!(CASTER_BYTES <= CASTER_STRIDE) };
    }

    #[test]
    fn the_shader_holds_as_many_maps_as_the_contract_allows() {
        let declared = format!("const SHADOW_MAPS: u32 = {MAX_SHADOW_MAPS}u;");
        assert!(include_str!("../shader/toon.wgsl").contains(&declared), "toon.wgsl declares {declared}");
    }

    #[test]
    fn quality_picks_size_and_softness_and_off_has_none() {
        assert_eq!(settings(ShadowQuality::Off), None);
        assert_eq!(settings(ShadowQuality::Low), Some((1024, 1.0)));
        assert_eq!(settings(ShadowQuality::High), Some((2048, 2.0)));
    }
}
