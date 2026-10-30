//! Ambient occlusion and contact shadows (#2507), drawn by the `Opaque` pass after its surfaces: `Scene::occlusion`
//! asks for creases, folds and contacts to darken. One fullscreen draw over the scene colour reads the scene depth as a
//! texture (its first sample, when multisampled), rebuilds each pixel's position and normal from it, and multiplies the
//! colour by how open the pixel is (`shader/occlusion.wgsl`):
//!
//! - **Ambient occlusion**: a horizon search in eight directions out to `radius` metres, each sample weighted by how
//!   far above the pixel's tangent plane it rises and how near it is (HBAO).
//! - **Contact shadows**: a march of sixteen steps from the pixel toward the first shadow-casting directional light, out
//!   to `contact` metres; a step that passes behind drawn depth (within a thickness) is in shadow.
//!
//! Multiplying the lit colour darkens direct light as well as ambient; for the small folds this is for, that is the
//! look wanted. A perspective camera only: an orthographic one skips it.

use bytemuck::{Pod, Zeroable};
use omuzaki_render_api::{Light, Occlusion, Projection};

use super::PassCx;
use crate::gpu::SCENE_FORMAT;
use crate::shared::Shared;

/// Mirrors `struct Params` in `shader/occlusion.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    depth: [f32; 4],
    lens: [f32; 4],
    shape: [f32; 4],
    light: [f32; 4],
}

/// The pipelines, the parameters' buffer and its group.
pub(super) struct AmbientOcclusion {
    params: wgpu::Buffer,
    params_group: wgpu::BindGroup,
    layouts: [wgpu::PipelineLayout; 2],
    /// Indexed by `[multisampled]`, each for the sample count it was built at, compiled on first use.
    pipelines: [Option<(u32, wgpu::RenderPipeline)>; 2],
}

impl AmbientOcclusion {
    pub(super) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("occlusion"),
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
            label: Some("occlusion"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("occlusion"),
            layout: &params_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }],
        });
        let layouts = [0, 1].map(|multisampled| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("occlusion"),
                bind_group_layouts: &[Some(&params_layout), Some(&shared.layouts.depth_input[multisampled])],
                immediate_size: 0,
            })
        });
        Self { params, params_group, layouts, pipelines: [None, None] }
    }

    /// Darkens what the opaque surfaces left by their occlusion. Returns whether it drew.
    pub(super) fn record(&mut self, cx: &mut PassCx<'_>) -> bool {
        let Some(occlusion) = cx.frame.scene.occlusion else { return false };
        let Some(params) = params(cx, &occlusion) else { return false };
        let (shared, samples) = (cx.shared, cx.frame.samples);
        let multisampled = samples > 1;
        let slot = &mut self.pipelines[usize::from(multisampled)];
        if slot.as_ref().is_none_or(|(built, _)| *built != samples) {
            *slot = Some((samples, pipeline(shared, &self.layouts[usize::from(multisampled)], multisampled, samples)));
        }
        let Some((_, pipeline)) = slot.as_ref() else { return false };
        shared.gpu.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let depth = cx
            .attachments
            .depth_input(&shared.gpu.device, &shared.layouts.depth_input[usize::from(multisampled)])
            .clone();
        let Some(mut pass) = cx.attachments.overlay_pass(cx.encoder, "occlusion") else { return false };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.params_group, &[]);
        pass.set_bind_group(1, &depth, &[]);
        pass.draw(0..3, 0..1);
        cx.stats.draw_calls += 1;
        true
    }
}

/// The frame's parameters: the projection's depth coefficients (as the edge lines read them), the lens, the reach and
/// strength, and the first shadow-casting directional light in the camera's frame. `None` for an orthographic camera.
fn params(cx: &PassCx<'_>, occlusion: &Occlusion) -> Option<Params> {
    let camera = &cx.frame.scene.camera;
    if !matches!(camera.projection, Projection::Perspective { .. }) {
        return None;
    }
    let internal = cx.frame.internal_size.as_vec2();
    let projection = camera.projection_matrix(internal.x / internal.y.max(1.0));
    let view = camera.view();
    let light = cx.frame.scene.lights.iter().find_map(|l| match *l {
        Light::Directional { to_light, casts_shadow: true, .. } => {
            Some(view.transform_vector3(to_light).normalize_or_zero())
        }
        _ => None,
    });
    let (light, on) = light.map_or((omuzaki_core::glam::Vec3::Z, 0.0), |l| (l, 1.0));
    // The frame index varies the sampling pattern from frame to frame, so a supersampled frame averages it out.
    #[allow(clippy::cast_precision_loss)] // only its low bits matter
    let seed = (cx.frame.scene.frame.index % 1024) as f32 + cx.round as f32 * 0.37;
    Some(Params {
        depth: [projection.w_axis.w, projection.w_axis.z, projection.z_axis.w, projection.z_axis.z],
        lens: [1.0 / projection.x_axis.x, 1.0 / projection.y_axis.y, internal.x, internal.y],
        shape: [occlusion.radius, occlusion.strength, occlusion.contact, seed],
        light: [light.x, light.y, light.z, on],
    })
}

fn pipeline(shared: &Shared, layout: &wgpu::PipelineLayout, multisampled: bool, samples: u32) -> wgpu::RenderPipeline {
    let device = &shared.gpu.device;
    let depth_type = if multisampled { "texture_depth_multisampled_2d" } else { "texture_depth_2d" };
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("occlusion"),
        source: wgpu::ShaderSource::Wgsl(
            include_str!("../shader/occlusion.wgsl").replace("DEPTH_TYPE", depth_type).into(),
        ),
    });
    // Multiply: the colour (premultiplied, so its alpha too would scale; the alpha is kept) times how open the pixel is.
    let colour = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::Src,
        operation: wgpu::BlendOperation::Add,
    };
    let alpha = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("occlusion"),
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
            entry_point: Some("fs_occlusion"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: SCENE_FORMAT,
                blend: Some(wgpu::BlendState { color: colour, alpha }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
