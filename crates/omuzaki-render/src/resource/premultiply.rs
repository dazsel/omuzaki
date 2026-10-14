//! The premultiply step for textures a GPU painter writes with straight alpha (`shader/premultiply.wgsl`).
//!
//! A dynamic straight-alpha texture is two textures: the *painter's*, `Rgba8Unorm` with `STORAGE_BINDING` (an sRGB format
//! cannot be a storage texture, and Vello renders only to one), and the *sampled* one the materials read, `Rgba8UnormSrgb`
//! (or `Rgba8Unorm` for linear data) holding premultiplied colour with a mip chain. After a repaint this step converts the
//! repainted rectangle from the first to the second: one small render pass, per repaint and not per frame.

use crate::resource::mips::FULLSCREEN_TRIANGLE_VERTICES;
use omuzaki_render_api::PixelRect;

/// The two pipelines (one per sampled format) and the bind group layout they share. One per renderer; the pipelines are
/// made up front because a paint must not compile shaders.
pub(crate) struct Premultiplier {
    srgb: wgpu::RenderPipeline,
    linear: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

/// What converting one texture needs: the painter's texture to read and the sampled texture to write.
pub(crate) struct PremultiplyStep {
    source: wgpu::BindGroup,
    destination: wgpu::TextureView,
    srgb: bool,
}

impl Premultiplier {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("premultiply"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shader/premultiply.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("premultiply"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("premultiply"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str, format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("premultiply"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            srgb: pipeline("fs_srgb", wgpu::TextureFormat::Rgba8UnormSrgb),
            linear: pipeline("fs_linear", wgpu::TextureFormat::Rgba8Unorm),
            layout,
        }
    }

    /// The step that converts `painter` into `sampled` (made by the caller, because it owns the mip chain; the views here keep it alive).
    pub(crate) fn step(
        &self,
        device: &wgpu::Device,
        painter: &wgpu::Texture,
        sampled: &wgpu::Texture,
    ) -> PremultiplyStep {
        let srgb = sampled.format() == wgpu::TextureFormat::Rgba8UnormSrgb;
        let source = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("premultiply"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(
                    &painter.create_view(&wgpu::TextureViewDescriptor::default()),
                ),
            }],
        });
        let destination = sampled.create_view(&wgpu::TextureViewDescriptor {
            base_mip_level: 0,
            mip_level_count: Some(1),
            ..Default::default()
        });
        PremultiplyStep { source, destination, srgb }
    }

    /// Records the conversion of `dirty` (level 0 pixels; an empty rectangle records nothing).
    pub(crate) fn record(&self, encoder: &mut wgpu::CommandEncoder, step: &PremultiplyStep, dirty: PixelRect) {
        if dirty.width == 0 || dirty.height == 0 {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("premultiply"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &step.destination,
                depth_slice: None,
                resolve_target: None,
                // Keep what the repaint did not touch.
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(if step.srgb { &self.srgb } else { &self.linear });
        pass.set_bind_group(0, &step.source, &[]);
        pass.set_scissor_rect(dirty.x, dirty.y, dirty.width, dirty.height);
        pass.draw(0..FULLSCREEN_TRIANGLE_VERTICES, 0..1);
    }
}
