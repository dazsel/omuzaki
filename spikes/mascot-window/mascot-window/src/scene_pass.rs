//! Draws `scene.wgsl`, the Spout spike's premultiplied test picture, and reads it back as a [`Scene`].
//!
//! The window renders this pass into an offscreen texture every frame; [`render_reference`] runs the same pass once on a
//! device without a window, to get the pixels the per-pixel click-through mask and the fringe reference are built from.

use bytemuck::{Pod, Zeroable};

use crate::scene::Scene;

/// The picture's shader: an unmodified copy of `spikes/broadcast-out/spout-out/src/scene.wgsl`.
pub const SHADER: &str = include_str!("scene.wgsl");

/// Format of the offscreen picture. Not sRGB: the numbers in the shader are display-encoded and stored as they are.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Smallest size that holds the picture's tag rows (64 cells of 16 px wide, 16 rows of 16 px tall), the same limits the
/// Spout spike enforces.
pub const MIN_SIZE: (u32, u32) = (1024, 256);

/// Uniform block; field order and padding mirror `Params` in `scene.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    size: [f32; 2],
    t: f32,
    frame: u32,
    ticks_hi: u32,
    ticks_lo: u32,
    truth: u32,
    pad: u32,
    bg: [f32; 4],
}

/// Why the reference picture could not be made.
#[derive(Debug, thiserror::Error)]
pub enum ReferenceError {
    /// The requested size cannot hold the picture's tag rows.
    #[error("picture size {width}x{height} is below the {}x{} minimum", MIN_SIZE.0, MIN_SIZE.1)]
    TooSmall {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// No adapter can run the picture.
    #[error("no GPU adapter for the reference picture: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    /// The adapter refused the device.
    #[error("opening the GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    /// Copying the pixels back failed.
    #[error("reading the picture back: {0}")]
    Readback(String),
}

/// The pipeline that draws the picture into a [`FORMAT`] target.
pub struct ScenePass {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
}

impl ScenePass {
    /// Builds the pipeline on `device`.
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        // No blending: the shader writes the final premultiplied pixel over a transparent clear.
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene params"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene params"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }],
        });
        Self { pipeline, bind_group, params }
    }

    /// Records the picture for a `size` target. `t` moves the circling disc and `frame` the frame stamp; the mascot
    /// window freezes both so its click-through mask matches the pixels, the cost run animates both.
    #[allow(clippy::cast_precision_loss)] // sizes are far below f32's exact integer range
    pub fn record(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        (t, frame): (f32, u32),
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let uniforms = Uniforms {
            size: [size.0 as f32, size.1 as f32],
            t,
            frame,
            ticks_hi: 0,
            ticks_lo: 0,
            truth: 0,
            pad: 0,
            bg: [0.0, 0.0, 0.0, 1.0],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&uniforms));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// A texture the picture can be drawn into and sampled or copied from.
pub fn target(device: &wgpu::Device, size: (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene target"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Draws the frozen picture (`t` = 0, frame 0) at `size` on a device without a window and reads it back.
///
/// # Errors
/// See [`ReferenceError`].
pub fn render_reference(
    backends: wgpu::Backends,
    fallback_adapter: bool,
    size: (u32, u32),
) -> Result<Scene, ReferenceError> {
    if size.0 < MIN_SIZE.0 || size.1 < MIN_SIZE.1 {
        return Err(ReferenceError::TooSmall { width: size.0, height: size.1 });
    }
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    let instance = wgpu::Instance::new(descriptor);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        force_fallback_adapter: fallback_adapter,
        ..Default::default()
    }))?;
    let (device, queue) = pollster::block_on(
        adapter
            .request_device(&wgpu::DeviceDescriptor { label: Some("mascot-window reference"), ..Default::default() }),
    )?;
    let pass = ScenePass::new(&device);
    let texture = target(&device, size);
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("reference") });
    pass.record(&queue, &mut encoder, &view, size, (0.0, 0), None);
    let row = size.0 * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("reference readback"),
        size: u64::from(padded) * u64::from(size.1),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(size.1),
            },
        },
        wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        // The receiver outlives the wait below; a send failure only means the reader gave up.
        let _ = tx.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| ReferenceError::Readback(e.to_string()))?;
    rx.recv()
        .map_err(|e| ReferenceError::Readback(e.to_string()))?
        .map_err(|e| ReferenceError::Readback(e.to_string()))?;
    let data = buffer.slice(..).get_mapped_range().map_err(|e| ReferenceError::Readback(e.to_string()))?;
    let mut pixels = Vec::with_capacity((row * size.1) as usize);
    for chunk in data.chunks(padded as usize).take(size.1 as usize) {
        pixels.extend_from_slice(&chunk[..row as usize]);
    }
    drop(data);
    buffer.unmap();
    Ok(Scene { width: size.0, height: size.1, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(SHADER).unwrap();
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap();
    }

    #[test]
    fn uniform_block_matches_the_shader_layout() {
        assert_eq!(size_of::<Uniforms>(), 48);
    }

    #[test]
    fn too_small_sizes_are_refused_before_the_gpu_is_touched() {
        assert!(matches!(
            render_reference(wgpu::Backends::empty(), false, (800, 600)),
            Err(ReferenceError::TooSmall { .. })
        ));
    }

    /// Needs a GPU or a software Vulkan driver (lavapipe); reports and passes where there is none.
    #[test]
    fn known_patches_hold_their_premultiplied_bytes() {
        let size = (1280, 720);
        let scene = match render_reference(wgpu::Backends::all(), false, size) {
            Ok(scene) => scene,
            Err(ReferenceError::Adapter(e)) => {
                eprintln!("skipped: no adapter ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        let at = |x: u32, y: u32| {
            let i = ((y * size.0 + x) * 4) as usize;
            [scene.pixels[i], scene.pixels[i + 1], scene.pixels[i + 2], scene.pixels[i + 3]]
        };
        // Patch origins: x = 40 + 64 * index, y = height - 96, 48 px square. Stored bytes are straight colour * alpha.
        let probe = |index: u32| at(40 + index * 64 + 24, size.1 - 96 + 24);
        let near = |got: [u8; 4], want: [u8; 4]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1);
        assert!(near(probe(0), [128, 64, 0, 128]), "{:?}", probe(0));
        assert!(near(probe(1), [0, 38, 64, 64]), "{:?}", probe(1));
        assert!(near(probe(2), [26, 26, 26, 26]), "{:?}", probe(2));
        assert!(near(probe(3), [255, 0, 0, 255]), "{:?}", probe(3));
        assert_eq!(probe(4), [0, 0, 0, 0]);
        // Tag rows are opaque; every stored colour is at most its alpha (premultiplied).
        assert_eq!(at(0, 0)[3], 255);
        assert!(scene.pixels.as_chunks::<4>().0.iter().all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
    }
}
