//! The resolve pass: Vello's straight-alpha texture to the face layer's premultiplied one.
//!
//! The pass writes what [`SkiaRasterizer`](crate::raster::SkiaRasterizer) writes, premultiplied sRGB values, so the two
//! rasterisers can stand in for each other (Q1 (a), #1433). Only mip level 0 is written: the renderer's `PaintTextures`
//! pass rebuilds the chain over the dirty rectangle. Only a rectangle of it is written: the source holds that rectangle
//! rendered from (0, 0), and the pass puts it back at its place, leaving the rest of the target as it was.

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
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
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

    /// The buffer the pass reads the rectangle's origin from. Write it with [`set_origin`](Self::set_origin).
    pub fn origin_buffer(device: &wgpu::Device) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("face-resolve-origin"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Tells the next pass that its source holds the rectangle at `(x, y)` of the target, rendered from (0, 0).
    pub fn set_origin(queue: &wgpu::Queue, origin: &wgpu::Buffer, x: u32, y: u32) {
        let mut bytes = [0_u8; 16];
        bytes[..4].copy_from_slice(&x.to_le_bytes());
        bytes[4..8].copy_from_slice(&y.to_le_bytes());
        queue.write_buffer(origin, 0, &bytes);
    }

    /// As [`set_origin`](Self::set_origin), for a source that holds the rectangle somewhere else than at (0, 0): the
    /// target pixel `p` reads the source texel `p - origin`, so an origin may be negative.
    pub fn set_origin_signed(queue: &wgpu::Queue, origin: &wgpu::Buffer, [x, y]: [i64; 2]) {
        let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
        let mut bytes = [0_u8; 16];
        bytes[..4].copy_from_slice(&clamp(x).to_le_bytes());
        bytes[4..8].copy_from_slice(&clamp(y).to_le_bytes());
        queue.write_buffer(origin, 0, &bytes);
    }

    /// A bind group that reads `source`, which must hold the rectangle `origin` names.
    pub fn bind(&self, device: &wgpu::Device, source: &wgpu::TextureView, origin: &wgpu::Buffer) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("face-resolve"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(source) },
                wgpu::BindGroupEntry { binding: 1, resource: origin.as_entire_binding() },
            ],
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

    /// Records the pass: every pixel of `level_zero` inside `rect` (`x`, `y`, width, height) is overwritten with the
    /// premultiplied `source` pixel; the rest is kept. `timestamps` (a query set and the first of two indices) times the
    /// pass on the GPU, for the timing probe.
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::BindGroup,
        level_zero: &wgpu::TextureView,
        rect: [u32; 4],
        timestamps: Option<(&wgpu::QuerySet, u32)>,
    ) {
        let timestamp_writes = timestamps.map(|(query_set, at)| wgpu::RenderPassTimestampWrites {
            query_set,
            beginning_of_pass_write_index: Some(at),
            end_of_pass_write_index: Some(at + 1),
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("face-resolve"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: level_zero,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            timestamp_writes,
            ..wgpu::RenderPassDescriptor::default()
        });
        pass.set_scissor_rect(rect[0], rect[1], rect[2], rect[3]);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, source, &[]);
        pass.draw(0..3, 0..1);
    }
}
