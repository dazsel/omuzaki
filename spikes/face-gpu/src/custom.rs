//! The custom coverage rasteriser's GPU half: one render pass into the renderer's own texture, one instanced quad per command.

use crate::geom::Prepared;
use bytemuck::cast_slice;

pub struct Custom {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    edges: Option<(wgpu::Buffer, u64)>,
    cmds: Option<(wgpu::Buffer, u64)>,
    indices: Option<(wgpu::Buffer, u64)>,
    bands: Option<(wgpu::Buffer, u64)>,
    bind: Option<wgpu::BindGroup>,
}

impl Custom {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("face-fill"),
            source: wgpu::ShaderSource::Wgsl(include_str!("fill.wgsl").into()),
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                storage(0),
                storage(1),
                storage(3),
                storage(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let over = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("face-fill"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(over),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, layout, globals, edges: None, cmds: None, indices: None, bands: None, bind: None }
    }

    fn grow(device: &wgpu::Device, slot: &mut Option<(wgpu::Buffer, u64)>, need: u64) -> bool {
        if slot.as_ref().is_some_and(|(_, cap)| *cap >= need) {
            return false;
        }
        let cap = need.next_power_of_two().max(4096);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: cap,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, cap));
        true
    }

    /// Draws `prep` into `target` (cleared to transparent first).
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::Texture,
        prep: &Prepared,
    ) {
        let (w, h) = (target.width(), target.height());
        let e_bytes: &[u8] = cast_slice(&prep.edges);
        let c_bytes: &[u8] = cast_slice(&prep.cmds);
        let i_bytes: &[u8] = cast_slice(&prep.indices);
        let b_bytes: &[u8] = cast_slice(&prep.bands);
        let g1 = Self::grow(device, &mut self.edges, e_bytes.len() as u64);
        let g2 = Self::grow(device, &mut self.cmds, c_bytes.len() as u64);
        let g3 = Self::grow(device, &mut self.indices, i_bytes.len() as u64);
        let g4 = Self::grow(device, &mut self.bands, b_bytes.len() as u64);
        if g1 || g2 || g3 || g4 || self.bind.is_none() {
            let (Some((eb, _)), Some((cb, _)), Some((ib, _)), Some((bb, _))) = (&self.edges, &self.cmds, &self.indices, &self.bands) else { return };
            self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: eb.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: cb.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.globals.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: ib.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: bb.as_entire_binding() },
                ],
            }));
        }
        if let (Some((eb, _)), Some((cb, _))) = (&self.edges, &self.cmds) {
            queue.write_buffer(eb, 0, e_bytes);
            queue.write_buffer(cb, 0, c_bytes);
        }
        if let (Some((ib, _)), Some((bb, _))) = (&self.indices, &self.bands) {
            queue.write_buffer(ib, 0, i_bytes);
            queue.write_buffer(bb, 0, b_bytes);
        }
        queue.write_buffer(&self.globals, 0, cast_slice(&[w as f32, h as f32, 0.0, 0.0]));
        let view = target.create_view(&wgpu::TextureViewDescriptor {
            mip_level_count: Some(1),
            ..Default::default()
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("face-fill") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("face-fill"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            if let Some(b) = &self.bind {
                pass.set_bind_group(0, b, &[]);
            }
            for i in 0..prep.bands.len() as u32 {
                pass.draw(0..4, i..i + 1);
            }
        }
        queue.submit([enc.finish()]);
    }
}
