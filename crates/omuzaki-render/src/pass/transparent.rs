//! The `Transparent` pass (R2e): blended instances (`AlphaMode::Blend`) drawn over the opaque scene, sorted
//! back to front, depth-tested but not writing depth.
//!
//! **Order.** `cx.frame.draws.for_pass(Pass::Transparent)` yields the blended instances in scene order; this pass sorts
//! them by `Instance::order` (lower first, the author's say), then by distance along the camera's view axis, farthest
//! first, then by scene index. The last key makes the order a total one, so a frame is a pure function of its scene and
//! never depends on the sort algorithm's stability. The distance is that of the centre of the instance's world-space
//! bounding box (the bind-pose box moved by the model, or the union of its joints' boxes for a skinned instance), the
//! same box the shadow fit uses. This is a per-instance order, as the contract promises: triangles of one instance are
//! not sorted against each other, and two instances that interpenetrate or wrap each other (hair around a head) are
//! ordered by their centres. Author `Instance::order` where that is not enough.
//!
//! **Pipeline.** The shaders are the Opaque pass's: `vs_main`, and `fs_blend` (`shader/transparent.wgsl`) which lights
//! the surface with `toon_colour` and the material's shading-program hooks and returns the albedo's alpha. A blended
//! hair program therefore looks like the opaque one, translucent. The blend is `src * a + dst * (1 - a)` for colour and
//! `a + dst_a * (1 - a)` for alpha, so the premultiplied scene colour stays premultiplied. Depth uses `LessEqual`
//! with writes off: a blended surface hides behind opaque ones and never hides anything itself. Sidedness and winding
//! follow the material and the transform exactly as in `Opaque`.
//!
//! **Allocation.** The sort vector is kept between frames, so a warm frame allocates nothing: the sort is in place
//! (`sort_unstable_by`, whose total order makes it deterministic). A frame with no blended instance leaves the pass
//! `Skipped` without opening a render pass.

use std::collections::HashMap;

use omuzaki_render_api::{Pass, RenderError};

use super::{FramePass, Outcome, PassCx};
use crate::gpu::{DEPTH_FORMAT, SCENE_FORMAT};
use crate::program::{AttributeSpec, MAX_ATTRIBUTES, ProgramSpec, Registry, opaque_source};
use crate::resource::material::GpuMaterial;
use crate::resource::mesh::{GpuMesh, extra_attribute, extra_layout, vertex_layouts};
use crate::shared::Shared;

/// The blend entry point, appended to the Opaque pass's module source.
const BLEND_SOURCE: &str = include_str!("../shader/transparent.wgsl");

/// What distinguishes one pipeline from another: the state that varies per draw (the Opaque pass's keys).
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct Variant {
    double_sided: bool,
    /// The transform flips winding, so the front face is clockwise.
    mirrored: bool,
    samples: u32,
    /// The shading program, as its index in the registry.
    program: Option<usize>,
    /// Bit `i`: the mesh supplies attribute `i` of the program.
    extras_present: u8,
}

/// One blended draw and what it is sorted by.
#[derive(Copy, Clone)]
struct Sorted {
    /// `Instance::order`: lower draws first.
    order: i16,
    /// Distance of the instance's box centre along the view axis, in metres. Larger is farther.
    depth: f32,
    /// Index into the frame's instance buffer, and the unique last sort key.
    instance: u32,
    mesh: u32,
    material: u32,
    mirrored: bool,
}

/// Back to front: the author's order, then farthest first, then scene order. A total order (`total_cmp`, and the
/// instance index is unique), so the sort needs no stability and a frame is deterministic.
fn back_to_front(a: &Sorted, b: &Sorted) -> std::cmp::Ordering {
    a.order.cmp(&b.order).then_with(|| b.depth.total_cmp(&a.depth)).then_with(|| a.instance.cmp(&b.instance))
}

/// See the module docs.
pub(crate) struct Transparent {
    registry: Option<&'static Registry>,
    /// One module per program (`None`: no program), compiled the first time a material needs it.
    modules: HashMap<Option<usize>, wgpu::ShaderModule>,
    layout: wgpu::PipelineLayout,
    /// Compiled on first use and kept.
    pipelines: HashMap<Variant, wgpu::RenderPipeline>,
    /// Sixteen bytes of zeros, bound with stride 0 to every extra stream a mesh does not supply.
    zeros: wgpu::Buffer,
    /// This frame's blended draws in drawing order; cleared, not freed, each frame.
    sorted: Vec<Sorted>,
}

impl Transparent {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        Self {
            registry: Registry::global().ok(),
            modules: HashMap::new(),
            pipelines: HashMap::new(),
            zeros: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("transparent zero extras"),
                size: 16,
                usage: wgpu::BufferUsages::VERTEX,
                mapped_at_creation: false,
            }),
            layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("transparent"),
                bind_group_layouts: &[
                    Some(&shared.layouts.frame),
                    Some(&shared.layouts.material),
                    Some(&shared.layouts.shadow_receiver),
                    Some(&shared.layouts.environment),
                ],
                immediate_size: 0,
            }),
            sorted: Vec::new(),
        }
    }

    fn spec(&self, program: Option<usize>) -> Option<&'static ProgramSpec> {
        program.and_then(|index| self.registry.and_then(|r| r.get(index)))
    }

    fn variant(&self, mesh: &GpuMesh, material: &GpuMaterial, mirrored: bool, samples: u32) -> Variant {
        let attributes: &[AttributeSpec] = self.spec(material.program).map_or(&[], |s| s.attributes);
        Variant {
            double_sided: material.material.double_sided,
            mirrored,
            samples,
            program: material.program,
            extras_present: mesh.extra_mask(attributes),
        }
    }

    /// Compiles the module of `program` (the Opaque source plus the blend entry point) unless an earlier frame did.
    fn ensure_module(&mut self, device: &wgpu::Device, program: Option<usize>) {
        let spec = self.spec(program);
        self.modules.entry(program).or_insert_with(|| {
            let source = format!("{}\n{BLEND_SOURCE}", opaque_source(spec));
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("transparent"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        });
    }

    fn build(&self, device: &wgpu::Device, variant: Variant) -> Option<wgpu::RenderPipeline> {
        let module = self.modules.get(&variant.program)?;
        let attributes: &[AttributeSpec] = self.spec(variant.program).map_or(&[], |s| s.attributes);
        let slots: [(wgpu::VertexAttribute, u8, bool); MAX_ATTRIBUTES] = std::array::from_fn(|i| {
            let components = attributes.get(i).map_or(4, |a| a.components);
            (extra_attribute(i, components), components, variant.extras_present >> i & 1 == 1)
        });
        let mut buffers = vertex_layouts().to_vec();
        buffers.extend(
            slots.iter().map(|(attribute, components, present)| Some(extra_layout(attribute, *components, *present))),
        );
        let over = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let alpha = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, ..over };
        Some(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("transparent"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &buffers,
            },
            primitive: wgpu::PrimitiveState {
                front_face: if variant.mirrored { wgpu::FrontFace::Cw } else { wgpu::FrontFace::Ccw },
                cull_mode: (!variant.double_sided).then_some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState { count: variant.samples, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_blend"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState { color: over, alpha }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        }))
    }

    /// Fills `self.sorted` with the frame's blended draws in drawing order.
    fn sort_draws(&mut self, cx: &PassCx<'_>) {
        self.sorted.clear();
        let scene = cx.frame.scene;
        let view = scene.camera.view();
        for draw in cx.frame.draws.for_pass(Pass::Transparent) {
            let (Some(mesh), Some(instance)) =
                (cx.resources.meshes.at(draw.mesh), scene.instances.get(draw.instance as usize))
            else {
                continue;
            };
            let palette = instance.skin.and_then(|range| {
                let start = range.start as usize;
                scene.joints.get(start..start.checked_add(range.len as usize)?)
            });
            let bounds = mesh.bounds.instance(&instance.transform, palette);
            let centre =
                if bounds.is_empty() { instance.transform.w_axis.truncate() } else { (bounds.min + bounds.max) * 0.5 };
            // Camera space looks down -Z, so distance is -z.
            let depth = -view.transform_point3(centre).z;
            self.sorted.push(Sorted {
                order: instance.order,
                depth,
                instance: draw.instance,
                mesh: draw.mesh,
                material: draw.material,
                mirrored: draw.mirrored,
            });
        }
        self.sorted.sort_unstable_by(back_to_front);
    }
}

impl FramePass for Transparent {
    fn pass(&self) -> Pass {
        Pass::Transparent
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        self.sort_draws(cx);
        if self.sorted.is_empty() {
            return Ok(Outcome::Skipped);
        }
        // Taken out for the frame so the pipelines can be built (`&mut self`) while it is read; put back below, so its
        // capacity is kept.
        let sorted = std::mem::take(&mut self.sorted);
        // Build any pipeline the frame needs before the render pass borrows the encoder.
        let device = &cx.shared.gpu.device;
        for entry in &sorted {
            let (Some(mesh), Some(material)) =
                (cx.resources.meshes.at(entry.mesh), cx.resources.materials.at(entry.material))
            else {
                continue;
            };
            let variant = self.variant(mesh, material, entry.mirrored, cx.frame.samples);
            if !self.pipelines.contains_key(&variant) {
                self.ensure_module(device, variant.program);
                if let Some(pipeline) = self.build(device, variant) {
                    self.pipelines.insert(variant, pipeline);
                }
            }
        }

        let environment = cx.environment_group();
        let mut pass = cx.attachments.geometry_pass(cx.encoder, "transparent", cx.frame.clear_colour());
        pass.set_bind_group(0, cx.frame.bind_group, &[]);
        pass.set_bind_group(2, cx.attachments.shadow.receiver_group(), &[]);
        pass.set_bind_group(3, environment, &[]);
        for entry in &sorted {
            let (Some(mesh), Some(material)) =
                (cx.resources.meshes.at(entry.mesh), cx.resources.materials.at(entry.material))
            else {
                continue;
            };
            let variant = self.variant(mesh, material, entry.mirrored, cx.frame.samples);
            let Some(pipeline) = self.pipelines.get(&variant) else { continue };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, &material.bind_group, &[]);
            let attributes: &[AttributeSpec] = self.spec(variant.program).map_or(&[], |s| s.attributes);
            mesh.draw_with_extras(
                &mut pass,
                entry.instance,
                cx.resources.morphs.shapes(entry.instance),
                attributes,
                &self.zeros,
            );
            cx.stats.draw_calls += 1;
            cx.stats.triangles += mesh.triangles;
        }
        drop(pass);
        self.sorted = sorted;
        Ok(Outcome::Ran)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(order: i16, depth: f32, instance: u32) -> Sorted {
        // Only the sort keys matter here.
        Sorted { order, depth, instance, mesh: 0, material: 0, mirrored: false }
    }

    fn sorted(mut v: Vec<Sorted>) -> Vec<u32> {
        v.sort_unstable_by(back_to_front);
        v.iter().map(|s| s.instance).collect()
    }

    #[test]
    fn farther_draws_first() {
        assert_eq!(sorted(vec![entry(0, 1.0, 0), entry(0, 3.0, 1), entry(0, 2.0, 2)]), [1, 2, 0]);
    }

    #[test]
    fn order_beats_distance() {
        assert_eq!(sorted(vec![entry(1, 9.0, 0), entry(0, 1.0, 1), entry(-2, 0.5, 2)]), [2, 1, 0]);
    }

    #[test]
    fn exact_ties_keep_scene_order_whatever_the_input_order() {
        let a = sorted(vec![entry(0, 2.0, 5), entry(0, 2.0, 3), entry(0, 2.0, 9), entry(0, 2.0, 1)]);
        assert_eq!(a, [1, 3, 5, 9]);
        let b = sorted(vec![entry(0, 2.0, 9), entry(0, 2.0, 1), entry(0, 2.0, 5), entry(0, 2.0, 3)]);
        assert_eq!(a, b);
    }

    #[test]
    fn nan_distance_still_gives_a_total_order() {
        assert_eq!(
            sorted(vec![entry(0, f32::NAN, 0), entry(0, 1.0, 1), entry(0, f32::NAN, 2)]),
            sorted(vec![entry(0, f32::NAN, 2), entry(0, 1.0, 1), entry(0, f32::NAN, 0)])
        );
    }
}
