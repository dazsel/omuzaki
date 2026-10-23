//! Edge lines (#2319), drawn by the `Outlines` pass after its hulls: `Scene::edges` asks for a band on the near side of
//! every depth jump, which the hulls miss where geometry sits close behind a silhouette (a chin over its neck). One
//! fullscreen draw over the scene colour reads the scene depth as a texture (its first sample, when multisampled) and
//! blends the line colour by `EdgeLines::coverage`, which `shader/edges.wgsl` ports; the mock runs the contract's own
//! function, and a conformance check holds the two together.

use bytemuck::{Pod, Zeroable};
use omuzaki_render_api::EdgeLines;

use super::PassCx;
use crate::gpu::SCENE_FORMAT;
use crate::shared::Shared;

/// Mirrors `struct Params` in `shader/edges.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    colour: [f32; 4],
    depth: [f32; 4],
    shape: [f32; 4],
}

/// The pipelines, the parameters' buffer and its group.
pub(super) struct Edges {
    params: wgpu::Buffer,
    params_group: wgpu::BindGroup,
    layouts: [wgpu::PipelineLayout; 2],
    /// Indexed by `[multisampled]`, each for the sample count it was built at, compiled on first use.
    pipelines: [Option<(u32, wgpu::RenderPipeline)>; 2],
}

impl Edges {
    pub(super) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("edge lines"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("edge lines"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("edge lines"),
            layout: &params_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }],
        });
        let layouts = [0, 1].map(|multisampled| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("edge lines"),
                bind_group_layouts: &[Some(&params_layout), Some(&shared.layouts.depth_input[multisampled])],
                immediate_size: 0,
            })
        });
        Self { params, params_group, layouts, pipelines: [None, None] }
    }

    /// Draws the scene's edge lines over what the geometry passes left. Returns whether it drew.
    pub(super) fn record(&mut self, cx: &mut PassCx<'_>) -> bool {
        let Some(edges) = cx.frame.scene.edges else { return false };
        let (shared, samples) = (cx.shared, cx.frame.samples);
        let multisampled = samples > 1;
        let slot = &mut self.pipelines[usize::from(multisampled)];
        if slot.as_ref().is_none_or(|(built, _)| *built != samples) {
            *slot = Some((samples, pipeline(shared, &self.layouts[usize::from(multisampled)], multisampled, samples)));
        }
        let Some((_, pipeline)) = slot.as_ref() else { return false };
        shared.gpu.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params(cx, &edges)));
        let depth =
            cx.attachments.depth_input(&shared.gpu.device, &shared.layouts.depth_input[usize::from(multisampled)]).clone();
        let Some(mut pass) = cx.attachments.overlay_pass(cx.encoder, "edge lines") else { return false };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.params_group, &[]);
        pass.set_bind_group(1, &depth, &[]);
        pass.draw(0..3, 0..1);
        cx.stats.draw_calls += 1;
        true
    }
}

/// The frame's parameters: the premultiplied colour, the projection's depth coefficients (as depth of field reads
/// them), and the band's width in internal pixels.
fn params(cx: &PassCx<'_>, edges: &EdgeLines) -> Params {
    let internal = cx.frame.internal_size.as_vec2();
    let projection = cx.frame.scene.camera.projection_matrix(internal.x / internal.y.max(1.0));
    // Internal pixels per output pixel: the band is `width_px` wide in the picture the caller sees.
    #[allow(clippy::cast_precision_loss)] // picture sizes are far below 2^24
    let scale = internal.y / (cx.output_desc.size.y.max(1) as f32);
    let alpha = edges.colour.alpha();
    let linear = edges.colour.to_linear() * alpha;
    Params {
        colour: [linear.x, linear.y, linear.z, alpha],
        depth: [projection.w_axis.w, projection.w_axis.z, projection.z_axis.w, projection.z_axis.z],
        shape: [edges.width_px * scale, edges.step, internal.x, internal.y],
    }
}

fn pipeline(shared: &Shared, layout: &wgpu::PipelineLayout, multisampled: bool, samples: u32) -> wgpu::RenderPipeline {
    let device = &shared.gpu.device;
    let depth_type = if multisampled { "texture_depth_multisampled_2d" } else { "texture_depth_2d" };
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("edge lines"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shader/edges.wgsl").replace("DEPTH_TYPE", depth_type).into()),
    });
    // Premultiplied over: the line covers `coverage` of the pixel.
    let over = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("edge lines"),
        layout: Some(layout),
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
            entry_point: Some("fs_edges"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: SCENE_FORMAT,
                blend: Some(wgpu::BlendState { color: over, alpha: over }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
