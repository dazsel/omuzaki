//! The `Shadows` pass: a fitted, depth-only map of the casters for each of the scene's shadow-casting directional lights.
//!
//! **Which lights.** Each directional light with `casts_shadow` owns a map, in light order, up to the quality's
//! [`max_maps`](omuzaki_render_api::ShadowQuality::max_maps): one at `Low` (the live default keeps one map's cost), up
//! to [`MAX_SHADOW_MAPS`](omuzaki_render_api::limits::MAX_SHADOW_MAPS) at `High`. Casting lights past that are lit
//! without shadow. Each further map costs one more depth-only drawing of the casters and nine more comparisons where a
//! lit surface faces that light.
//!
//! **What is drawn.** `cx.frame.draws.for_pass(Pass::Shadows)`: the instances that cast. Single-sided surfaces draw
//! their *back* faces into the map, as three.js does (the prototype's look), so a lit face never competes with its own
//! depth and acne stays away from it; double-sided surfaces draw both. Masked and blended materials discard like the
//! lit pass does (`shader/shadow.wgsl`). Skinned instances are posed by the same `pose` as the lit passes.
//!
//! **The frustum.** Fitted per map, every redraw of it, to the casters' world bounds (skinned ones included) and the receivers' depth
//! (`shadow::fit`; opaque and blended receivers both count): a texel-snapped window around the casters' bounding sphere, stepped in scale, so nothing shimmers.
//!
//! **Live interval.** In `QualityMode::Live` each map is redrawn every `Quality::shadow_interval`-th frame and a frame
//! that redraws none reports the pass as skipped, while `Opaque` keeps sampling the maps and the matrices of their last
//! redraws. A first frame, a changed light direction or a new map size redraws a map at once. `QualityMode::Offline` redraws every frame, so a
//! frame depends on its own inputs only.
//!
//! **Looking it up.** The maps and their settings are published to the lit passes as group 2 (`Attachments::shadow`);
//! the lookup is `shadow_visibility` in `shader/toon.wgsl`, which finds a directional light's map by its slot. With
//! shadows off, or no caster, sampling is switched off and the pass is skipped.

use std::collections::HashMap;

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{AlphaMode, Light, Pass, RenderError};

use super::{FramePass, Outcome, PassCx};
use crate::gpu::DEPTH_FORMAT;
use crate::resource::mesh::vertex_layouts;
use crate::shadow::bounds::MeshBounds;
use crate::shadow::fit::{self, Aabb};
use crate::shadow::map::{self, NORMAL_BIAS};
use crate::shared::Shared;

const SOURCE: &str = concat!(include_str!("../shader/common.wgsl"), include_str!("../shader/shadow.wgsl"));

/// What distinguishes one caster pipeline from another.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct Variant {
    double_sided: bool,
    /// The transform flips winding, so the front face is clockwise.
    mirrored: bool,
    /// The material discards texels (masked or blended), so the fragment stage runs.
    cutout: bool,
}

/// See the module docs.
pub(crate) struct Shadows {
    module: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    /// Compiled on first use and kept.
    pipelines: HashMap<Variant, wgpu::RenderPipeline>,
}

/// The directional lights that cast, in light order: each one's slot and the direction toward it.
fn casting_lights(lights: &[Light]) -> impl Iterator<Item = (usize, Vec3)> + '_ {
    lights.iter().take(omuzaki_render_api::limits::MAX_LIGHTS).enumerate().filter_map(|(slot, light)| match *light {
        Light::Directional { to_light, casts_shadow: true, .. } => Some((slot, to_light)),
        _ => None,
    })
}

/// Whether `scene` under `quality` fits a shadow map: a light casts and the quality has maps. A frame that does reads the
/// bounds of every caster and receiver, and one that does not never reads them for shadows.
pub(crate) fn fits_shadows(scene: &omuzaki_render_api::Scene, quality: &omuzaki_render_api::Quality) -> bool {
    map::settings(quality.shadows).is_some() && casting_lights(&scene.lights).next().is_some()
}

impl Shadows {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        Self {
            module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shadows"),
                source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
            }),
            layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shadows"),
                bind_group_layouts: &[
                    Some(&shared.layouts.frame),
                    Some(&shared.layouts.material),
                    Some(&shared.layouts.shadow_caster),
                ],
                immediate_size: 0,
            }),
            pipelines: HashMap::new(),
        }
    }

    fn build(&self, device: &wgpu::Device, variant: Variant) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadows"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &vertex_layouts(),
            },
            primitive: wgpu::PrimitiveState {
                front_face: if variant.mirrored { wgpu::FrontFace::Cw } else { wgpu::FrontFace::Ccw },
                // Back faces into the map for single-sided surfaces (see the module docs).
                cull_mode: (!variant.double_sided).then_some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: variant.cutout.then(|| wgpu::FragmentState {
                module: &self.module,
                entry_point: Some("fs_cutout"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[],
            }),
            multiview_mask: None,
            cache: None,
        })
    }
}

/// The world bounds of the casters and, separately, of the surfaces that receive shadow.
fn scene_bounds(cx: &PassCx<'_>) -> (Aabb, Aabb) {
    let (scene, resources) = (cx.frame.scene, cx.resources);
    let bounds_of = |draw: &crate::frame::draws::Draw| {
        let (instance, mesh) = (scene.instances.get(draw.instance as usize)?, resources.meshes.at(draw.mesh)?);
        let palette = instance.skin.and_then(|range| {
            let start = range.start as usize;
            scene.joints.get(start..start + range.len as usize)
        });
        Some(MeshBounds::instance(
            resources.morphs.bounds(draw.instance).unwrap_or(&mesh.bounds),
            &instance.transform,
            palette,
        ))
    };
    let mut casters = Aabb::EMPTY;
    for draw in cx.frame.draws.for_pass(Pass::Shadows) {
        if let Some(b) = bounds_of(draw) {
            casters.union(&b);
        }
    }
    let mut receivers = Aabb::EMPTY;
    // Blended surfaces receive too: the `shadow-catcher` ground is one, and without it the depth range would stop short.
    for draw in cx.frame.draws.for_pass(Pass::Opaque).chain(cx.frame.draws.for_pass(Pass::Transparent)) {
        if resources.materials.at(draw.material).is_some_and(|m| m.material.receives_shadows)
            && let Some(b) = bounds_of(draw)
        {
            receivers.union(&b);
        }
    }
    (casters, receivers)
}

impl FramePass for Shadows {
    fn pass(&self) -> Pass {
        Pass::Shadows
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        let queue = &cx.shared.gpu.queue;
        let most = cx.frame.quality.shadows.max_maps();
        let count = casting_lights(&cx.frame.scene.lights).take(most).count();
        let (Some((size, spacing)), true) = (map::settings(cx.frame.quality.shadows), count > 0) else {
            cx.attachments.shadow.disable_from(queue, 0);
            return Ok(Outcome::Skipped);
        };
        if cx.frame.draws.for_pass(Pass::Shadows).next().is_none() {
            cx.attachments.shadow.disable_from(queue, 0);
            return Ok(Outcome::Skipped);
        }
        cx.attachments.shadow.resize(cx.shared, size, count);
        cx.attachments.shadow.disable_from(queue, count);

        // The bounds are read once, by the first map due this frame.
        let mut bounds = None;
        let mut ran = false;
        for (index, (slot, to_light)) in casting_lights(&cx.frame.scene.lights).take(count).enumerate() {
            if !cx.attachments.shadow.due(index, cx.frame.quality, slot, to_light) {
                continue;
            }
            let (casters, receivers) = *bounds.get_or_insert_with(|| scene_bounds(cx));
            let Some(fitted) = fit::fit(to_light, &casters, &receivers, size, NORMAL_BIAS) else {
                cx.attachments.shadow.disable(queue, index);
                continue;
            };
            cx.attachments.shadow.publish(queue, index, &fitted, slot, spacing, to_light);
            self.draw_map(cx, index);
            ran = true;
        }
        Ok(if ran { Outcome::Ran } else { Outcome::Skipped })
    }
}

impl Shadows {
    /// Draws the casters into map `index`, whose matrix was just published.
    fn draw_map(&mut self, cx: &mut PassCx<'_>, index: usize) {
        // Build any pipeline the frame needs before the render pass borrows the encoder.
        let device = &cx.shared.gpu.device;
        for draw in cx.frame.draws.for_pass(Pass::Shadows) {
            let Some(material) = cx.resources.materials.at(draw.material) else { continue };
            let variant = Variant {
                double_sided: material.material.double_sided,
                mirrored: draw.mirrored,
                cutout: !matches!(material.material.alpha, AlphaMode::Opaque),
            };
            if !self.pipelines.contains_key(&variant) {
                let pipeline = self.build(device, variant);
                self.pipelines.insert(variant, pipeline);
            }
        }

        let frame_group = cx.frame_group();
        let Some(mut pass) = cx.attachments.shadow.begin_pass(cx.encoder, index) else { return };
        let (caster_group, offset) = cx.attachments.shadow.caster_group(index);
        pass.set_bind_group(0, frame_group, &[]);
        pass.set_bind_group(2, caster_group, &[offset]);
        for draw in cx.frame.draws.for_pass(Pass::Shadows) {
            let (Some(mesh), Some(material)) =
                (cx.resources.meshes.at(draw.mesh), cx.resources.materials.at(draw.material))
            else {
                continue;
            };
            let variant = Variant {
                double_sided: material.material.double_sided,
                mirrored: draw.mirrored,
                cutout: !matches!(material.material.alpha, AlphaMode::Opaque),
            };
            let Some(pipeline) = self.pipelines.get(&variant) else { continue };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, &material.bind_group, &[]);
            mesh.draw(&mut pass, draw.instance, cx.resources.morphs.shapes(draw.instance));
            cx.stats.draw_calls += 1;
            cx.stats.triangles += mesh.triangles;
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_render_api::{Colour, Light};

    use super::*;

    fn sun(casts_shadow: bool) -> Light {
        Light::Directional { to_light: Vec3::Y, colour: Colour::WHITE, intensity: 1.0, casts_shadow }
    }

    #[test]
    fn every_casting_directional_light_gets_a_map_in_light_order() {
        let lights = [
            Light::Hemisphere { sky: Colour::WHITE, ground: Colour::BLACK, intensity: 1.0 },
            sun(false),
            Light::Directional { to_light: Vec3::X, colour: Colour::WHITE, intensity: 1.0, casts_shadow: true },
            sun(true),
        ];
        assert_eq!(casting_lights(&lights).collect::<Vec<_>>(), [(2, Vec3::X), (3, Vec3::Y)]);
        assert_eq!(casting_lights(&[sun(false)]).next(), None);
        assert_eq!(casting_lights(&[]).next(), None);
    }

    #[test]
    fn a_light_past_the_frames_limit_is_not_a_caster() {
        let mut lights = vec![sun(false); omuzaki_render_api::limits::MAX_LIGHTS];
        lights.push(sun(true));
        assert_eq!(casting_lights(&lights).next(), None);
    }
}
