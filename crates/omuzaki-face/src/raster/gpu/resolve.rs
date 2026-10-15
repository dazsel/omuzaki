//! The resolve pass: Vello's straight-alpha texture to the face layer's premultiplied one.
//!
//! The pass writes what [`SkiaRasterizer`](crate::raster::SkiaRasterizer) writes, premultiplied sRGB values, so the two
//! rasterisers can stand in for each other (Q1 (a), #1433). Only mip level 0 is written: the renderer's `PaintTextures`
//! pass rebuilds the chain over the dirty rectangle.

/// The pipeline that copies a straight-alpha source into a premultiplied target of one format.
pub struct Resolve {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
}

impl Resolve {
    /// Compiles the pass for targets of `format`.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("face-resolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("resolve.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("face-resolve"),
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
            label: Some("face-resolve"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("face-resolve"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, layout, format }
    }

    /// The target format this pass was compiled for.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// A bind group that reads `source`, which must stay the same size as the targets it is resolved into.
    pub fn bind(&self, device: &wgpu::Device, source: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("face-resolve"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(source) }],
        })
    }

    /// A view of `target`'s level 0 alone, which is all [`encode`](Self::encode) writes. Keep it while the texture lives:
    /// making one allocates.
    pub fn level_zero(target: &wgpu::Texture) -> wgpu::TextureView {
        target.create_view(&wgpu::TextureViewDescriptor {
            mip_level_count: Some(1),
            ..wgpu::TextureViewDescriptor::default()
        })
    }

    /// Records the pass: every pixel of `level_zero` is overwritten with the premultiplied `source` pixel.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, source: &wgpu::BindGroup, level_zero: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("face-resolve"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: level_zero,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..wgpu::RenderPassDescriptor::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, source, &[]);
        pass.draw(0..3, 0..1);
    }
}
