//! The Vello candidate: `DrawList` -> `vello::Scene`, rendered to an Rgba8Unorm storage texture, then copied into the target.

use omuzaki_core::glam::Affine2;
use omuzaki_face_api::{Cap, Cmd, DrawList, FaceLayerSpec, Join, Paint, PathEl};
use vello::kurbo::{self, Affine, BezPath, Cap as KCap, Join as KJoin, Point, Stroke};
use vello::peniko::{self, Color, Fill, color::AlphaColor};
use vello::{AaConfig, RenderParams, Renderer, RendererOptions, Scene};

pub struct VelloPath {
    pub renderer: Renderer,
    pub scene: Scene,
    storage: Option<(wgpu::Texture, wgpu::TextureView)>,
    resolve: Option<(wgpu::RenderPipeline, wgpu::BindGroupLayout)>,
}

fn pt(p: omuzaki_core::glam::Vec2) -> Point {
    Point::new(f64::from(p.x), f64::from(p.y))
}

fn affine(a: Affine2) -> Affine {
    let c = a.to_cols_array();
    Affine::new(c.map(f64::from))
}

fn bez(els: &[PathEl]) -> BezPath {
    let mut p = BezPath::new();
    for el in els {
        match *el {
            PathEl::Move(a) => p.move_to(pt(a)),
            PathEl::Line(a) => p.line_to(pt(a)),
            PathEl::Quad(a, b) => p.quad_to(pt(a), pt(b)),
            PathEl::Cubic(a, b, c) => p.curve_to(pt(a), pt(b), pt(c)),
            PathEl::Close => p.close_path(),
        }
    }
    p
}

fn colour(c: omuzaki_core::colour::Colour, opacity: f32) -> Color {
    AlphaColor::from_rgba8(c.r, c.g, c.b, ((f32::from(c.a) * opacity).round().clamp(0.0, 255.0)) as u8)
}

fn brush(p: &Paint, opacity: f32) -> peniko::Brush {
    let stops = |s: &omuzaki_face_api::Stops| -> Vec<peniko::ColorStop> {
        s.as_slice()
            .iter()
            .map(|g| peniko::ColorStop { offset: g.offset, color: colour(g.colour, opacity).into() })
            .collect()
    };
    // Canvas interpolates straight (unpremultiplied) colour, which is what the CPU rasteriser reproduces.
    let straight = |mut g: peniko::Gradient| {
        g.interpolation_alpha_space = peniko::InterpolationAlphaSpace::Unpremultiplied;
        peniko::Brush::Gradient(g)
    };
    match p {
        Paint::Solid(c) => peniko::Brush::Solid(colour(*c, opacity)),
        Paint::Linear { from, to, stops: s } => {
            straight(peniko::Gradient::new_linear(pt(*from), pt(*to)).with_stops(stops(s).as_slice()))
        }
        Paint::Radial { centre, radius, stops: s } => {
            straight(peniko::Gradient::new_radial(pt(*centre), *radius).with_stops(stops(s).as_slice()))
        }
    }
}

impl VelloPath {
    pub fn new(device: &wgpu::Device) -> Result<Self, String> {
        let renderer = Renderer::new(
            device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: std::num::NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|e| format!("{e}"))?;
        Ok(Self { renderer, scene: Scene::new(), storage: None, resolve: None })
    }

    pub fn build(&mut self, list: &DrawList, spec: &FaceLayerSpec, px_per_m: f32) {
        self.scene.reset();
        let (w, h) = spec.rect.size_px(px_per_m);
        let (sx, sy) = (w as f32 / spec.rect.width(), h as f32 / spec.rect.height());
        let to_px = Affine2::from_cols(
            omuzaki_core::glam::Vec2::new(sx, 0.0),
            omuzaki_core::glam::Vec2::new(0.0, -sy),
            omuzaki_core::glam::Vec2::new(-spec.rect.x0 * sx, spec.rect.y1 * sy),
        );
        let mut xf = vec![to_px];
        for cmd in list.cmds() {
            let cur = *xf.last().unwrap_or(&to_px);
            match *cmd {
                Cmd::PushTransform(t) => xf.push(cur * t),
                Cmd::PopTransform => {
                    xf.pop();
                }
                Cmd::PushClip(p) => {
                    self.scene.push_clip_layer(Fill::NonZero, affine(cur), &bez(list.elements(p)));
                }
                Cmd::PopClip => self.scene.pop_layer(),
                Cmd::Fill { path, paint, opacity } => {
                    self.scene.fill(Fill::NonZero, affine(cur), &brush(&paint, opacity), None, &bez(list.elements(path)));
                }
                Cmd::Stroke { path, paint, width, cap, join, opacity } => {
                    let mut s = Stroke::new(f64::from(width));
                    s.start_cap = if cap == Cap::Round { KCap::Round } else { KCap::Butt };
                    s.end_cap = s.start_cap;
                    s.join = if join == Join::Round { KJoin::Round } else { KJoin::Miter };
                    s.miter_limit = 10.0;
                    self.scene.stroke(&s, affine(cur), &brush(&paint, opacity), None, &bez(list.elements(path)));
                }
            }
        }
        let _ = kurbo::Affine::IDENTITY;
    }

    /// Renders the scene into Vello's storage texture and copies it into `target`.
    pub fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::Texture) -> Result<(), String> {
        let (w, h) = (target.width(), target.height());
        if self.storage.as_ref().is_none_or(|(t, _)| t.width() != w || t.height() != h) {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("vello-out"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let v = t.create_view(&wgpu::TextureViewDescriptor::default());
            self.storage = Some((t, v));
        }
        let Some((tex, view)) = &self.storage else { return Err("no storage".into()) };
        self.renderer
            .render_to_texture(
                device,
                queue,
                &self.scene,
                view,
                &RenderParams { base_color: Color::TRANSPARENT, width: w, height: h, antialiasing_method: AaConfig::Area },
            )
            .map_err(|e| format!("{e}"))?;
        // Vello writes straight alpha; the face texture is premultiplied, so a full-screen pass converts and copies.
        if self.resolve.is_none() {
            self.resolve = Some(make_resolve(device, target.format()));
        }
        let Some((pipe, layout)) = &self.resolve else { return Err("no resolve".into()) };
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) }],
        });
        let tview = target.create_view(&wgpu::TextureViewDescriptor { mip_level_count: Some(1), ..Default::default() });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vello-resolve") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tview,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([enc.finish()]);
        let _ = tex;
        Ok(())
    }
}

fn make_resolve(device: &wgpu::Device, format: wgpu::TextureFormat) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            "@group(0) @binding(0) var src: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(p.xy), 0);
    return vec4<f32>(c.rgb * c.a, c.a);
}"
            .into(),
        ),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
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
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
    let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pl),
        vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    (pipe, layout)
}
