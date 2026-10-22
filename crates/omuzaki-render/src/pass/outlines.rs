//! The `Outlines` pass (LK-02): inverted-hull lines around the instances whose material has an outline.
//!
//! The mesh is drawn a second time with every vertex pushed out along its smooth normal ([`GpuMesh`]'s second vertex
//! stream, the normals of vertices within a micrometre of each other averaged so a hard-edged mesh stays closed) by
//! `Outline::world_width`, and only the faces turned away from the camera are kept. Drawn after `Opaque` into the same
//! colour and depth, the hull shows as a band around the silhouette and creases and hides behind the surface
//! everywhere else. The colour is `Outline::colour` times `Scene::outline_light`, so lines dim with the scene and
//! never glow (unless the lighting mix asks for the surface's light). `shader/outline.wgsl` has the shader.
//!
//! The material's `OutlineStyle` says what the width measures (#647, `MToon`'s outline). By default (the kit's line) it is
//! world metres scaled with distance (`clamp(depth / 2 m, 0.6, 3)`), so a character that steps back keeps a readable line
//! that follows the model's own scale; `World` is metres at any distance; `Screen` is a fraction of the target's height,
//! pushed in clip space, the same on screen at any distance. A width texture multiplies the width per texel (its green
//! channel, read in the vertex stage), and the lighting mix tints the line toward the surface's own lit colour, so the
//! pass binds the lit passes' shadow and environment groups. A material's `alpha` mask cuts the hull where it cuts the
//! surface (#649), as three-vrm's `MToon` outline does, so a cut-out card's line follows its visible shape instead of
//! the whole card. A width of 0 draws nothing, and a frame with no outlined instance leaves the pass `Skipped` without
//! opening a render pass.

use std::collections::HashMap;

use omuzaki_render_api::{Pass, RenderError};

use super::{FramePass, Outcome, PassCx};
use crate::frame::FrameView;
use crate::gpu::{DEPTH_FORMAT, SCENE_FORMAT};
use crate::program::opaque_source;
use crate::resource::SceneResources;
use crate::resource::material::GpuMaterial;
use crate::resource::mesh::{GpuMesh, smooth_normal_layout, vertex_layouts};
use crate::shared::Shared;

/// The Opaque pass's toon shader with no program (which the lighting mix lights the surface with), then the hull's.
fn source() -> String {
    format!("{}\n{}", opaque_source(None), include_str!("../shader/outline.wgsl"))
}

/// What distinguishes one pipeline from another: the state that varies per draw. The hull always culls the front
/// faces, whatever the material's sidedness: a double-sided sheet is outlined by its back faces too.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct Variant {
    /// The transform flips winding, so the front face is clockwise.
    mirrored: bool,
    samples: u32,
}

/// One hull to draw: an instance with a mesh and a material whose outline has a width.
struct Hull<'a> {
    instance: u32,
    mesh: &'a GpuMesh,
    material: &'a GpuMaterial,
    variant: Variant,
}

/// The outlined instances of the frame, in scene order. Instances whose outline is 0 wide are left out: a hull of no
/// thickness would still leave a hairline where its back faces meet the front ones at the silhouette.
fn hulls<'a>(frame: &'a FrameView<'_>, resources: SceneResources<'a>) -> impl Iterator<Item = Hull<'a>> {
    frame.draws.for_pass(Pass::Outlines).filter_map(move |draw| {
        let mesh = resources.meshes.at(draw.mesh)?;
        let material = resources.materials.at(draw.material)?;
        if !material.draws_outline() {
            return None;
        }
        Some(Hull {
            instance: draw.instance,
            mesh,
            material,
            variant: Variant { mirrored: draw.mirrored, samples: frame.samples },
        })
    })
}

/// See the module docs.
pub(crate) struct Outlines {
    module: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    /// Compiled on first use (a scene's first frame with outlines) and kept.
    pipelines: HashMap<Variant, wgpu::RenderPipeline>,
}

impl Outlines {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        Self {
            module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("outlines"),
                source: wgpu::ShaderSource::Wgsl(source().into()),
            }),
            layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("outlines"),
                bind_group_layouts: &[
                    Some(&shared.layouts.frame),
                    Some(&shared.layouts.material),
                    Some(&shared.layouts.shadow_receiver),
                    Some(&shared.layouts.environment),
                ],
                immediate_size: 0,
            }),
            pipelines: HashMap::new(),
        }
    }
}

fn build_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    variant: Variant,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("outlines"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_outline"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &{
                let [shape, surface] = vertex_layouts();
                [shape, surface, Some(smooth_normal_layout())]
            },
        },
        primitive: wgpu::PrimitiveState {
            front_face: if variant.mirrored { wgpu::FrontFace::Cw } else { wgpu::FrontFace::Ccw },
            // The hull is the model's back faces: the front ones are the model itself.
            cull_mode: Some(wgpu::Face::Front),
            ..Default::default()
        },
        // Writes depth like any opaque surface, so later passes (blending, depth of field) see the line where it is. No
        // depth bias: nudging the hull nearer shows it wherever it is almost coplanar with its own surface (grazing hair
        // sheets), which is z-fighting made visible, not a better line.
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState { count: variant.samples, ..Default::default() },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_outline"),
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

impl FramePass for Outlines {
    fn pass(&self) -> Pass {
        Pass::Outlines
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        let (frame, resources) = (cx.frame, cx.resources);
        // Build any pipeline the frame needs before the render pass borrows the encoder; and learn whether there is
        // anything to draw, since opening a render pass costs a load and a store of the whole scene.
        let device = &cx.shared.gpu.device;
        let mut any = false;
        for hull in hulls(frame, resources) {
            any = true;
            self.pipelines
                .entry(hull.variant)
                .or_insert_with(|| build_pipeline(device, &self.module, &self.layout, hull.variant));
        }
        if !any {
            return Ok(Outcome::Skipped);
        }

        let environment = cx.environment_group();
        let mut pass = cx.attachments.geometry_pass(cx.encoder, "outlines", frame.clear_colour());
        pass.set_bind_group(0, frame.bind_group, &[]);
        pass.set_bind_group(2, cx.attachments.shadow.receiver_group(), &[]);
        pass.set_bind_group(3, environment, &[]);
        for hull in hulls(frame, resources) {
            let Some(pipeline) = self.pipelines.get(&hull.variant) else { continue };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, &hull.material.bind_group, &[]);
            hull.mesh.draw_with_smooth_normals(
                &mut pass,
                hull.instance,
                resources.morphs.outline_streams(hull.instance),
            );
            cx.stats.draw_calls += 1;
            cx.stats.triangles += hull.mesh.triangles;
        }
        Ok(Outcome::Ran)
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_render_api::Outline;

    /// The shader's copy of `Outline::world_width`'s constants must be the contract's.
    #[test]
    fn shader_constants_match_the_contract() {
        let source = include_str!("../shader/outline.wgsl");
        for (name, value) in [
            ("OUTLINE_REFERENCE_DISTANCE", Outline::REFERENCE_DISTANCE),
            ("OUTLINE_MIN_SCALE", Outline::MIN_SCALE),
            ("OUTLINE_MAX_SCALE", Outline::MAX_SCALE),
        ] {
            let line = format!("const {name}: f32 = {value:?};");
            assert!(source.contains(&line), "outline.wgsl should declare `{line}`");
        }
    }
}
