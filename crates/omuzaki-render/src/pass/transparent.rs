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
//! with writes off: a blended surface hides behind opaque ones and never hides anything itself, unless its `MToon`
//! asks to write depth (`transparentWithZWrite`), when it hides its own far side and what is drawn after it. Sidedness and winding
//! follow the material and the transform exactly as in `Opaque`.
//!
//! **Order-independent surfaces** (`Material::order_independent`, #2507) are not sorted. After the sorted ones, they
//! draw in a pass of their own into two targets the attachments keep (`fs_order_independent`, weighted blended
//! order-independent transparency, `McGuire` and `Bavoil` 2013): the weighted sum of their premultiplied colour, blended
//! `src + dst`, and how much of what is behind shows through, blended `dst * (1 - src)`. Neither depends on the order
//! the layers arrive in, so overlapping hair tips or fur never pop as the view turns. One fullscreen draw
//! (`shader/oit.wgsl`) then lays their weighted average over the scene colour, covering it as much as the layers
//! together hide it, sample by sample when multisampled. The depth is tested, never written. An adapter without
//! independent blending, or without the sample count for the reveal target, sorts these surfaces with the rest.
//!
//! **Allocation.** The sort vector is kept between frames, so a warm frame allocates nothing: the sort is in place
//! (`sort_unstable_by`, whose total order makes it deterministic). A frame with no blended instance leaves the pass
//! `Skipped` without opening a render pass.

use std::collections::HashMap;

use omuzaki_render_api::{Pass, RenderError};

use super::{FramePass, FrameStats, Outcome, PassCx};
use crate::gpu::{DEPTH_FORMAT, OIT_REVEAL_FORMAT, SCENE_FORMAT};
use crate::program::{AttributeSpec, MAX_ATTRIBUTES, ProgramSpec, Registry, opaque_source};
use crate::resource::SceneResources;
use crate::resource::material::{GpuMaterial, mtoon_of};
use crate::resource::mesh::{GpuMesh, extra_attribute, extra_layout, vertex_layouts};
use crate::shared::Shared;

/// The blend entry points, appended to the Opaque pass's module source.
const BLEND_SOURCE: &str = include_str!("../shader/transparent.wgsl");

/// The order-independent layers' composite.
const COMPOSITE_SOURCE: &str = include_str!("../shader/oit.wgsl");

/// What distinguishes one pipeline from another: the state that varies per draw (the Opaque pass's keys).
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
#[allow(clippy::struct_excessive_bools, reason = "a pipeline key: each switch is its own pipeline state")]
struct Variant {
    double_sided: bool,
    /// The transform flips winding, so the front face is clockwise.
    mirrored: bool,
    samples: u32,
    /// The shading program, as its index in the registry.
    program: Option<usize>,
    /// Bit `i`: the mesh supplies attribute `i` of the program.
    extras_present: u8,
    /// The surface also writes depth (`Mtoon::blend_writes_depth`, `MToon`'s `transparentWithZWrite`).
    writes_depth: bool,
    /// Drawn into the order-independent targets rather than over the scene colour.
    order_independent: bool,
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
    /// Drawn order-independently, after the sorted draws.
    order_independent: bool,
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
    /// The order-independent layers' composite.
    composite: Composite,
}

/// The order-independent layers' composite: its bind group layouts and pipelines, indexed by `[multisampled]`, each
/// for the sample count it was built at and compiled on first use.
struct Composite {
    layouts: [wgpu::BindGroupLayout; 2],
    pipeline_layouts: [wgpu::PipelineLayout; 2],
    pipelines: [Option<(u32, wgpu::RenderPipeline)>; 2],
}

impl Composite {
    fn new(device: &wgpu::Device) -> Self {
        let layouts = [false, true].map(|multisampled| {
            let entry = |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled,
                },
                count: None,
            };
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("transparency composite"),
                entries: &[entry(0), entry(1)],
            })
        });
        let pipeline_layouts = std::array::from_fn(|i| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("transparency composite"),
                bind_group_layouts: &[Some(&layouts[i])],
                immediate_size: 0,
            })
        });
        Self { layouts, pipeline_layouts, pipelines: [None, None] }
    }

    /// The pipeline for `samples` per pixel, built unless an earlier frame did.
    fn pipeline(&mut self, device: &wgpu::Device, samples: u32) -> Option<&wgpu::RenderPipeline> {
        let multisampled = samples > 1;
        let slot = &mut self.pipelines[usize::from(multisampled)];
        if slot.as_ref().is_none_or(|(built, _)| *built != samples) {
            let target = if multisampled { "texture_multisampled_2d<f32>" } else { "texture_2d<f32>" };
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("transparency composite"),
                source: wgpu::ShaderSource::Wgsl(COMPOSITE_SOURCE.replace("TARGET_TYPE", target).into()),
            });
            // Premultiplied over: the layers' colour plus the scene's times what shows through.
            let over = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            };
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("transparency composite"),
                layout: Some(&self.pipeline_layouts[usize::from(multisampled)]),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_composite"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: SCENE_FORMAT,
                        blend: Some(wgpu::BlendState { color: over, alpha: over }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
            *slot = Some((samples, pipeline));
        }
        slot.as_ref().map(|(_, pipeline)| pipeline)
    }
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
            composite: Composite::new(device),
        }
    }

    fn spec(&self, program: Option<usize>) -> Option<&'static ProgramSpec> {
        program.and_then(|index| self.registry.and_then(|r| r.get(index)))
    }

    fn variant(&self, mesh: &GpuMesh, material: &GpuMaterial, entry: &Sorted, samples: u32) -> Variant {
        let attributes: &[AttributeSpec] = self.spec(material.program).map_or(&[], |s| s.attributes);
        Variant {
            double_sided: material.material.double_sided,
            mirrored: entry.mirrored,
            samples,
            program: material.program,
            extras_present: mesh.extra_mask(attributes),
            // Order-independent layers never hide each other.
            writes_depth: !entry.order_independent
                && mtoon_of(&material.material).is_some_and(|m| m.blend_writes_depth),
            order_independent: entry.order_independent,
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
        let sorted_targets = [Some(wgpu::ColorTargetState {
            format: SCENE_FORMAT,
            blend: Some(wgpu::BlendState { color: over, alpha }),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        // The weighted sum adds; the reveal multiplies by what each layer lets through.
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let through = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        };
        let independent_targets = [
            Some(wgpu::ColorTargetState {
                format: SCENE_FORMAT,
                blend: Some(wgpu::BlendState { color: add, alpha: add }),
                write_mask: wgpu::ColorWrites::ALL,
            }),
            Some(wgpu::ColorTargetState {
                format: OIT_REVEAL_FORMAT,
                blend: Some(wgpu::BlendState { color: through, alpha: through }),
                write_mask: wgpu::ColorWrites::RED,
            }),
        ];
        let (entry_point, targets): (_, &[_]) = if variant.order_independent {
            ("fs_order_independent", &independent_targets)
        } else {
            ("fs_blend", &sorted_targets)
        };
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
                depth_write_enabled: Some(variant.writes_depth),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState { count: variant.samples, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(entry_point),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets,
            }),
            multiview_mask: None,
            cache: None,
        }))
    }

    /// Fills `self.sorted` with the frame's blended draws in drawing order, the order-independent ones (which
    /// `independent` says the adapter can draw) marked.
    fn sort_draws(&mut self, cx: &PassCx<'_>, independent: bool) {
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
            let bounds = cx
                .resources
                .morphs
                .sort_bounds(draw.instance)
                .unwrap_or(&mesh.bounds)
                .instance(&instance.transform, palette);
            let centre =
                if bounds.is_empty() { instance.transform.w_axis.truncate() } else { (bounds.min + bounds.max) * 0.5 };
            // Camera space looks down -Z, so distance is -z.
            let depth = -view.transform_point3(centre).z;
            let order_independent =
                independent && cx.resources.materials.at(draw.material).is_some_and(|m| m.material.order_independent);
            self.sorted.push(Sorted {
                order: instance.order,
                depth,
                instance: draw.instance,
                mesh: draw.mesh,
                material: draw.material,
                mirrored: draw.mirrored,
                order_independent,
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
        let samples = cx.frame.samples;
        self.sort_draws(cx, cx.shared.gpu.order_independent(samples));
        if self.sorted.is_empty() {
            return Ok(Outcome::Skipped);
        }
        // Taken out for the frame so the pipelines can be built (`&mut self`) while it is read; put back below, so its
        // capacity is kept.
        let sorted = std::mem::take(&mut self.sorted);
        // Build any pipeline the frame needs before a render pass borrows the encoder.
        let device = &cx.shared.gpu.device;
        for entry in &sorted {
            let (Some(mesh), Some(material)) =
                (cx.resources.meshes.at(entry.mesh), cx.resources.materials.at(entry.material))
            else {
                continue;
            };
            let variant = self.variant(mesh, material, entry, samples);
            if !self.pipelines.contains_key(&variant) {
                self.ensure_module(device, variant.program);
                if let Some(pipeline) = self.build(device, variant) {
                    self.pipelines.insert(variant, pipeline);
                }
            }
        }
        // Cheap handle clones, so the groups stay bound while a render pass holds the encoder.
        let groups =
            [cx.frame_group(), cx.attachments.shadow.receiver_group(), cx.environment_group()].map(Clone::clone);
        let draws = Draws { resources: cx.resources, samples, groups: &groups };
        if sorted.iter().any(|e| !e.order_independent) {
            let mut pass = cx.attachments.geometry_pass(cx.encoder, "transparent", cx.frame.clear_colour());
            self.draw(&draws, &mut pass, &sorted, false, &mut cx.stats);
        }
        if sorted.iter().any(|e| e.order_independent) {
            let mut pass = cx.attachments.oit_pass(&cx.shared.gpu.device, cx.encoder);
            self.draw(&draws, &mut pass, &sorted, true, &mut cx.stats);
            drop(pass);
            self.composite(cx);
        }
        self.sorted = sorted;
        Ok(Outcome::Ran)
    }
}

/// What drawing the blended entries reads from the frame: the resources, the sample count and groups 0, 2 and 3.
struct Draws<'a> {
    resources: SceneResources<'a>,
    samples: u32,
    groups: &'a [wgpu::BindGroup; 3],
}

impl Transparent {
    /// Draws the entries of `sorted` that are (or are not) `order_independent` into `pass`, in order.
    fn draw(
        &self,
        draws: &Draws<'_>,
        pass: &mut wgpu::RenderPass<'_>,
        sorted: &[Sorted],
        order_independent: bool,
        stats: &mut FrameStats,
    ) {
        let [frame, shadow, environment] = draws.groups;
        pass.set_bind_group(0, frame, &[]);
        pass.set_bind_group(2, shadow, &[]);
        pass.set_bind_group(3, environment, &[]);
        let resources = draws.resources;
        for entry in sorted.iter().filter(|e| e.order_independent == order_independent) {
            let (Some(mesh), Some(material)) =
                (resources.meshes.at(entry.mesh), resources.materials.at(entry.material))
            else {
                continue;
            };
            let variant = self.variant(mesh, material, entry, draws.samples);
            let Some(pipeline) = self.pipelines.get(&variant) else { continue };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, &material.bind_group, &[]);
            let attributes: &[AttributeSpec] = self.spec(variant.program).map_or(&[], |s| s.attributes);
            mesh.draw_with_extras(
                pass,
                entry.instance,
                resources.morphs.shapes(entry.instance),
                attributes,
                &self.zeros,
            );
            stats.draw_calls += 1;
            stats.triangles += mesh.triangles;
        }
    }

    /// Lays the order-independent layers over the scene colour.
    fn composite(&mut self, cx: &mut PassCx<'_>) {
        let (device, samples) = (&cx.shared.gpu.device, cx.frame.samples);
        let multisampled = usize::from(samples > 1);
        let Some(input) = cx.attachments.oit_input(device, &self.composite.layouts[multisampled]).cloned() else {
            return;
        };
        let Some(pipeline) = self.composite.pipeline(device, samples) else { return };
        let Some(mut pass) = cx.attachments.overlay_pass(cx.encoder, "transparency composite") else { return };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &input, &[]);
        pass.draw(0..3, 0..1);
        cx.stats.draw_calls += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(order: i16, depth: f32, instance: u32) -> Sorted {
        // Only the sort keys matter here.
        Sorted { order, depth, instance, mesh: 0, material: 0, mirrored: false, order_independent: false }
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
