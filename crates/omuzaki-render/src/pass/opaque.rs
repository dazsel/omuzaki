//! The `Opaque` pass: opaque and alpha-masked instances into the scene colour and depth.
//!
//! Every instance is posed (skinned if it carries joints), culled by its material's sidedness with the winding flipped
//! for mirrored transforms, and shaded by `shader/toon.wgsl`: base colour, vertex colours, base texture and overlays
//! (`surface_albedo`), then the toon model for `Shading::Toon` (the terminator ramp with its tinted shade, the rim, the
//! skin treatment, hemisphere, directional and point lights) or the flat surface colour for `Shading::Unlit`. Masked
//! pixels below the cutoff are discarded. The pass always runs, because it opens the frame: it clears the background
//! and depth even when there is nothing to draw.
//!
//! The shadow map is bound at group 2 and looked up in the lighting (`shadow_visibility` in the shader).
//! A material that names a registered shading program (`program/mod.rs`) draws with a shader module of its own: the
//! same toon model with the program's two hooks, and the mesh's extra attributes bound as vertex streams. Pipelines are
//! keyed by program too, and by which of the program's attributes the mesh supplies. Blended instances are the `Transparent` pass's, which reuses the shader's `toon_colour`.

use std::collections::HashMap;

use omuzaki_render_api::{Pass, RenderError};

use super::{FramePass, Outcome, PassCx};
use crate::gpu::{DEPTH_FORMAT, SCENE_FORMAT};
use crate::program::{AttributeSpec, MAX_ATTRIBUTES, ProgramSpec, Registry, opaque_source};
use crate::resource::material::GpuMaterial;
use crate::resource::mesh::{GpuMesh, extra_attribute, extra_layout, vertex_layout};
use crate::shared::Shared;

/// What distinguishes one pipeline from another: the state that varies per draw.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct Variant {
    double_sided: bool,
    /// The transform flips winding, so the front face is clockwise.
    mirrored: bool,
    samples: u32,
    /// The shading program, as its index in the registry.
    program: Option<usize>,
    /// Bit `i`: the mesh supplies attribute `i` of the program (see `GpuMesh::extra_mask`).
    extras_present: u8,
}

/// See the module docs.
pub(crate) struct Opaque {
    registry: Option<&'static Registry>,
    /// One module per program (`None`: no program), compiled the first time a material needs it.
    modules: HashMap<Option<usize>, wgpu::ShaderModule>,
    layout: wgpu::PipelineLayout,
    /// Compiled on first use (a scene's first frame) and kept.
    pipelines: HashMap<Variant, wgpu::RenderPipeline>,
    /// Sixteen bytes of zeros, bound with stride 0 to every extra stream a mesh does not supply.
    zeros: wgpu::Buffer,
}

impl Opaque {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        Self {
            registry: Registry::global().ok(),
            modules: HashMap::new(),
            zeros: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("zero extras"),
                size: 16,
                usage: wgpu::BufferUsages::VERTEX,
                mapped_at_creation: false,
            }),
            layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("opaque"),
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

    /// The program a variant draws with.
    fn spec(&self, program: Option<usize>) -> Option<&'static ProgramSpec> {
        program.and_then(|index| self.registry.and_then(|r| r.get(index)))
    }

    /// Compiles the shader module of `program` unless an earlier frame already did.
    fn ensure_module(&mut self, device: &wgpu::Device, program: Option<usize>) {
        let spec = self.spec(program);
        self.modules.entry(program).or_insert_with(|| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("opaque"),
                source: wgpu::ShaderSource::Wgsl(opaque_source(spec).into()),
            })
        });
    }

    /// The pipeline for `variant`, compiled on first use. Its vertex streams are the mesh and the four extra slots: an
    /// attribute the mesh supplies is read per vertex, any other slot is a stride-0 stream of zeros.
    fn build(&self, device: &wgpu::Device, variant: Variant) -> Option<wgpu::RenderPipeline> {
        let module = self.modules.get(&variant.program)?;
        let attributes: &[AttributeSpec] = self.spec(variant.program).map_or(&[], |s| s.attributes);
        let slots: [(wgpu::VertexAttribute, u8, bool); MAX_ATTRIBUTES] = std::array::from_fn(|i| {
            let components = attributes.get(i).map_or(4, |a| a.components);
            (extra_attribute(i, components), components, variant.extras_present >> i & 1 == 1)
        });
        let mut buffers = vec![Some(vertex_layout())];
        buffers.extend(
            slots.iter().map(|(attribute, components, present)| Some(extra_layout(attribute, *components, *present))),
        );
        Some(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("opaque"),
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
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState { count: variant.samples, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        }))
    }
}

impl Opaque {
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
}

impl FramePass for Opaque {
    fn pass(&self) -> Pass {
        Pass::Opaque
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        // Build any pipeline the frame needs before the render pass borrows the encoder.
        let device = &cx.shared.gpu.device;
        for draw in cx.frame.draws.for_pass(Pass::Opaque) {
            let (Some(mesh), Some(material)) =
                (cx.resources.meshes.at(draw.mesh), cx.resources.materials.at(draw.material))
            else {
                continue;
            };
            let variant = self.variant(mesh, material, draw.mirrored, cx.frame.samples);
            if !self.pipelines.contains_key(&variant) {
                self.ensure_module(device, variant.program);
                if let Some(pipeline) = self.build(device, variant) {
                    self.pipelines.insert(variant, pipeline);
                }
            }
        }

        let environment = cx.environment_group();
        let mut pass = cx.attachments.geometry_pass(cx.encoder, "opaque", cx.frame.clear_colour());
        pass.set_bind_group(0, cx.frame.bind_group, &[]);
        pass.set_bind_group(2, cx.attachments.shadow.receiver_group(), &[]);
        pass.set_bind_group(3, environment, &[]);
        for draw in cx.frame.draws.for_pass(Pass::Opaque) {
            let (Some(mesh), Some(material)) =
                (cx.resources.meshes.at(draw.mesh), cx.resources.materials.at(draw.material))
            else {
                continue;
            };
            let variant = self.variant(mesh, material, draw.mirrored, cx.frame.samples);
            let Some(pipeline) = self.pipelines.get(&variant) else { continue };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, &material.bind_group, &[]);
            let attributes: &[AttributeSpec] = self.spec(variant.program).map_or(&[], |s| s.attributes);
            mesh.draw_with_extras(&mut pass, draw.instance, attributes, &self.zeros);
            cx.stats.draw_calls += 1;
            cx.stats.triangles += mesh.triangles;
        }
        Ok(Outcome::Ran)
    }
}
