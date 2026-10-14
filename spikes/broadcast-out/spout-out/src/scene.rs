//! The test scene: renders a premultiplied-alpha picture with a frame tag into a BGRA8 wgpu texture.
//!
//! BGRA8 because that is Spout's default shared-texture format (`DXGI_FORMAT_B8G8R8A8_UNORM`), so the texture can be
//! copied into Spout's without a conversion pass. Works on any wgpu backend; only the Spout hand-off needs Windows.

use bytemuck::{Pod, Zeroable};

use crate::frame_tag::{FOOTER_HEIGHT, FrameTag, HEADER_HEIGHT, MIN_HEIGHT, MIN_WIDTH};

/// Format of the render target: Spout's default sender format.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// The background the truth render composites over, as 0..255 RGB. A mid blue-grey so dark and bright fringes both show.
pub const TRUTH_BACKGROUND: [f32; 3] = [40.0, 44.0, 64.0];

/// Straight colour and alpha of the known-value patches, in the order the shader draws them (mirrors `scene.wgsl`).
/// Stored premultiplied, so what lands in the frame is `colour * alpha`.
pub const PATCHES: [([f32; 3], f32); 5] = [
    ([1.0, 0.5, 0.0], 0.5),
    ([0.0, 0.6, 1.0], 0.25),
    ([1.0, 1.0, 1.0], 0.1),
    ([1.0, 0.0, 0.0], 1.0),
    ([0.0, 0.0, 0.0], 0.0),
];

/// Centre pixel of patch `index` in a frame `height` pixels tall (the shader puts 48-pixel squares 96 rows from the bottom).
#[must_use]
pub fn patch_centre(index: usize, height: u32) -> (u32, u32) {
    (40 + 64 * u32::try_from(index).unwrap_or(0) + 24, height - 96 + 24)
}

/// The BGRA bytes patch `index` should hold: straight colour times alpha, rounded.
#[must_use]
pub fn expected_patch(index: usize) -> [u8; 4] {
    let ([r, g, b], a) = PATCHES[index];
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |v: f32| (v * 255.0).round() as u8;
    [byte(b * a), byte(g * a), byte(r * a), byte(a)]
}

/// Checks the patches of a tightly packed BGRA frame against [`expected_patch`], allowing one level for GPU rounding.
///
/// # Errors
/// Names the first patch whose bytes differ.
pub fn check_patches(bgra: &[u8], width: u32, height: u32) -> Result<(), String> {
    for index in 0..PATCHES.len() {
        let (x, y) = patch_centre(index, height);
        let at = (y as usize * width as usize + x as usize) * 4;
        let got = bgra.get(at..at + 4).ok_or_else(|| format!("patch {index} lies outside the frame"))?;
        let want = expected_patch(index);
        if got.iter().zip(want).any(|(g, w)| g.abs_diff(w) > 1) {
            return Err(format!("patch {index}: got {got:?}, want {want:?} (BGRA)"));
        }
    }
    Ok(())
}

/// What changes from frame to frame.
#[derive(Clone, Copy, Debug)]
pub struct SceneParams {
    /// The tag drawn into the header rows.
    pub tag: FrameTag,
    /// Animation time in radians for the moving disc.
    pub t: f32,
    /// Render the picture composited over [`TRUTH_BACKGROUND`] instead of over transparency.
    pub truth: bool,
}

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

/// Why a scene could not be made or read.
#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    /// The requested size cannot hold the frame tag.
    #[error("frame size {0}x{1} is below the {MIN_WIDTH}x{MIN_HEIGHT} minimum")]
    TooSmall(u32, u32),
    /// Copying pixels back from the GPU failed.
    #[error("GPU readback failed: {0}")]
    Readback(String),
}

/// Renders the scene into its own target texture and can read it back for checks.
pub struct SceneRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
}

impl SceneRenderer {
    /// Builds the pipeline and a `size` target on `device`.
    ///
    /// # Errors
    /// Fails when `size` is too small to carry the frame tag.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, size: (u32, u32)) -> Result<Self, SceneError> {
        if size.0 < MIN_WIDTH || size.1 < MIN_HEIGHT {
            return Err(SceneError::TooSmall(size.0, size.1));
        }
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
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
        // No blending: the shader writes the final premultiplied pixel, and the target starts transparent black.
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
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene target"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self { device: device.clone(), queue: queue.clone(), pipeline, bind_group, params, target, view, size })
    }

    /// The render target. Its contents are the last submitted frame.
    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.target
    }

    /// Size of the target in pixels.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Draws one frame and submits it. Does not wait for the GPU.
    pub fn render(&self, params: &SceneParams) -> wgpu::SubmissionIndex {
        #[allow(clippy::cast_precision_loss)] // sizes are far below f32's exact integer range
        let uniforms = Uniforms {
            size: [self.size.0 as f32, self.size.1 as f32],
            t: params.t,
            frame: params.tag.frame,
            ticks_hi: (params.tag.ticks >> 32) as u32,
            ticks_lo: u32::try_from(params.tag.ticks & 0xFFFF_FFFF).unwrap_or(0),
            truth: u32::from(params.truth),
            pad: 0,
            bg: [TRUTH_BACKGROUND[0] / 255.0, TRUTH_BACKGROUND[1] / 255.0, TRUTH_BACKGROUND[2] / 255.0, 1.0],
        };
        self.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&uniforms));
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scene frame") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()])
    }

    /// Copies the target to the CPU as tightly packed BGRA8 rows. Waits for the GPU; for checks, not the hot path.
    ///
    /// # Errors
    /// Fails when the GPU copy or the buffer mapping fails.
    pub fn read_back(&self) -> Result<Vec<u8>, SceneError> {
        let (w, h) = self.size;
        let row = w * 4;
        let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scene readback") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            // The receiver outlives the wait below; a send failure only means the reader gave up.
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| SceneError::Readback(e.to_string()))?;
        rx.recv().map_err(|e| SceneError::Readback(e.to_string()))?.map_err(|e| SceneError::Readback(e.to_string()))?;
        let data = buffer.slice(..).get_mapped_range().map_err(|e| SceneError::Readback(e.to_string()))?;
        let mut pixels = Vec::with_capacity((row * h) as usize);
        for chunk in data.chunks(padded as usize).take(h as usize) {
            pixels.extend_from_slice(&chunk[..row as usize]);
        }
        drop(data);
        buffer.unmap();
        Ok(pixels)
    }

    /// Rows of the picture area (between the header and the footer), for analysis that ignores the tag.
    #[must_use]
    pub fn picture_rows(&self) -> std::ops::Range<u32> {
        HEADER_HEIGHT..self.size.1 - FOOTER_HEIGHT
    }
}
