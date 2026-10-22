//! The environment backdrop ([`Background::Environment`]), drawn by the `Opaque` pass after its surfaces: one
//! triangle at the far plane per frame (`shader/backdrop.wgsl`, `environment/backdrop.rs`). Not a pass of its own,
//! because the contract's eight passes are fixed and it belongs where the background is made.

use std::collections::HashMap;

use omuzaki_render_api::Background;

use super::PassCx;
use crate::environment::backdrop::{BackdropUniform, GpuBackdrop};
use crate::environment::{GpuEnvironment, to_sky};
use crate::gpu::{DEPTH_FORMAT, SCENE_FORMAT};
use crate::shared::Shared;

/// The backdrop's shader and its pipelines, one per sample count, compiled on first use.
pub(super) struct BackdropDraw {
    module: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    pipelines: HashMap<u32, wgpu::RenderPipeline>,
}

impl BackdropDraw {
    pub(super) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        Self {
            module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("backdrop"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shader/backdrop.wgsl").into()),
            }),
            layout: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("backdrop"),
                bind_group_layouts: &[Some(&shared.layouts.backdrop)],
                immediate_size: 0,
            }),
            pipelines: HashMap::new(),
        }
    }

    /// When the frame shows the environment behind it: writes this frame's rays and intensity, makes sure the
    /// pipeline exists, and returns what [`draw`](Self::draw) needs. `None` for any other background. Call it before
    /// the render pass borrows the encoder.
    pub(super) fn prepare<'a>(&mut self, cx: &PassCx<'a>) -> Option<Prepared<'a>> {
        if cx.frame.scene.background != Background::Environment {
            return None;
        }
        let (shared, resources) = (cx.shared, cx.resources);
        let environment: &'a GpuEnvironment = cx
            .frame
            .draws
            .environment
            .and_then(|slot| resources.environments.at(slot))
            .unwrap_or(&shared.no_environment);
        let size = cx.frame.internal_size;
        #[allow(clippy::cast_precision_loss)] // picture sides are far below 2^24
        let aspect = size.x as f32 / size.y.max(1) as f32;
        let rays = cx.frame.scene.camera.sky_rays(aspect);
        // The stand-in shows black whatever the scene says, so its intensity doesn't matter.
        let uniform = cx.frame.scene.environment.map_or_else(
            || BackdropUniform::new(rays, 0.0),
            |using| BackdropUniform::new(to_sky(&using) * rays, using.intensity),
        );
        let backdrop = environment.backdrop();
        backdrop.write(&shared.gpu.queue, &uniform);
        let samples = cx.frame.samples;
        if !self.pipelines.contains_key(&samples) {
            let pipeline = self.build(&shared.gpu.device, samples);
            self.pipelines.insert(samples, pipeline);
        }
        Some(Prepared { backdrop, samples })
    }

    /// Draws the backdrop into the open scene pass.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, prepared: &Prepared<'_>) {
        let Some(pipeline) = self.pipelines.get(&prepared.samples) else { return };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, prepared.backdrop.bind_group(), &[]);
        pass.draw(0..3, 0..1);
    }

    fn build(&self, device: &wgpu::Device, samples: u32) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("backdrop"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            // At the far plane, where the depth was cleared to: only pixels no surface covers pass.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &self.module,
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
        })
    }
}

/// A frame's backdrop, ready to draw.
pub(super) struct Prepared<'a> {
    backdrop: &'a GpuBackdrop,
    samples: u32,
}
