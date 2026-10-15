//! The `Shadows` pass: a fitted, depth-only map of the casters for the scene's shadow-casting directional light.
//!
//! **Which light.** The first directional light with `casts_shadow` owns the map; further casting lights are lit
//! without shadow for now.
//!
//! **What is drawn.** `cx.frame.draws.for_pass(Pass::Shadows)`: the instances that cast. Single-sided surfaces draw
//! their *back* faces into the map, as three.js does (the prototype's look), so a lit face never competes with its own
//! depth and acne stays away from it; double-sided surfaces draw both. Masked and blended materials discard like the
//! lit pass does (`shader/shadow.wgsl`). Skinned instances are posed by the same `pose` as the lit passes.
//!
//! **The frustum.** Fitted every redraw to the casters' world bounds (skinned ones included) and the receivers' depth
//! (`shadow::fit`; opaque and blended receivers both count): a texel-snapped window around the casters' bounding sphere, stepped in scale, so nothing shimmers.
//!
//! **Live interval.** In `QualityMode::Live` the map is redrawn every `Quality::shadow_interval`-th frame and the frames
//! between report the pass as skipped, while `Opaque` keeps sampling the map and the matrix of the last redraw. A first
//! frame, a changed light direction or a new map size redraws at once. `QualityMode::Offline` redraws every frame, so a
//! frame depends on its own inputs only.
//!
//! **Looking it up.** The map and its settings are published to the lit passes as group 2 (`Attachments::shadow`);
//! the lookup is `shadow_visibility` in `shader/toon.wgsl`. With shadows off, or no caster, sampling is switched off and
//! the pass is skipped.

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

/// The first directional light that casts: its slot and the direction toward it.
fn casting_light(lights: &[Light]) -> Option<(usize, Vec3)> {
    lights.iter().take(omuzaki_render_api::limits::MAX_LIGHTS).enumerate().find_map(|(slot, light)| match *light {
        Light::Directional { to_light, casts_shadow: true, .. } => Some((slot, to_light)),
        _ => None,
    })
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
        Some(MeshBounds::instance(&mesh.bounds, &instance.transform, palette))
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
        let (Some((size, spacing)), Some((slot, to_light))) =
            (map::settings(cx.frame.quality.shadows), casting_light(&cx.frame.scene.lights))
        else {
            cx.attachments.shadow.disable(queue);
            return Ok(Outcome::Skipped);
        };
        if cx.frame.draws.for_pass(Pass::Shadows).next().is_none() {
            cx.attachments.shadow.disable(queue);
            return Ok(Outcome::Skipped);
        }
        cx.attachments.shadow.resize(cx.shared, size);
        if !cx.attachments.shadow.due(cx.frame.quality, to_light) {
            return Ok(Outcome::Skipped);
        }

        let (casters, receivers) = scene_bounds(cx);
        let Some(fitted) = fit::fit(to_light, &casters, &receivers, size, NORMAL_BIAS) else {
            cx.attachments.shadow.disable(queue);
            return Ok(Outcome::Skipped);
        };
        cx.attachments.shadow.publish(queue, &fitted, slot, spacing, to_light);

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

        let mut pass = cx.attachments.shadow.begin_pass(cx.encoder);
        pass.set_bind_group(0, cx.frame.bind_group, &[]);
        pass.set_bind_group(2, cx.attachments.shadow.caster_group(), &[]);
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
            mesh.draw(&mut pass, draw.instance);
            cx.stats.draw_calls += 1;
            cx.stats.triangles += mesh.triangles;
        }
        Ok(Outcome::Ran)
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
    fn the_first_casting_directional_light_owns_the_map() {
        let lights = [
            Light::Hemisphere { sky: Colour::WHITE, ground: Colour::BLACK, intensity: 1.0 },
            sun(false),
            Light::Directional { to_light: Vec3::X, colour: Colour::WHITE, intensity: 1.0, casts_shadow: true },
            sun(true),
        ];
        assert_eq!(casting_light(&lights), Some((2, Vec3::X)));
        assert_eq!(casting_light(&[sun(false)]), None);
        assert_eq!(casting_light(&[]), None);
    }

    #[test]
    fn a_light_past_the_frames_limit_is_not_a_caster() {
        let mut lights = vec![sun(false); omuzaki_render_api::limits::MAX_LIGHTS];
        lights.push(sun(true));
        assert_eq!(casting_light(&lights), None);
    }
}
