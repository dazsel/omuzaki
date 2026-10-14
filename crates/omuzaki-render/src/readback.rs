//! Copying a finished target to the CPU.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image, RenderError};

use crate::gpu::{Gpu, WAIT_LIMIT};

/// Reads mip level `level` of `texture` (RGBA8, `size` pixels at that level) back, waiting for the GPU for at most
/// [`WAIT_LIMIT`]. For tests, goldens and CPU outputs: it stalls the pipeline, so the live path shares the texture
/// instead.
///
/// # Errors
/// `Backend` if the GPU or the mapping does not finish in time, or the buffer cannot be read.
pub(crate) fn read_rgba8(
    gpu: &Gpu,
    texture: &wgpu::Texture,
    level: u32,
    size: UVec2,
    alpha: AlphaEncoding,
) -> Result<Image, RenderError> {
    let row = size.x as usize * 4;
    // Copies need each row padded to a multiple of 256 bytes.
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * size.y as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u32::try_from(padded).unwrap_or(u32::MAX)),
                rows_per_image: Some(size.y),
            },
        },
        wgpu::Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([encoder.finish()]);

    let (sender, receiver) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        // The receiver outlives the wait below; a failed send only means the reader gave up.
        let _ = sender.send(result);
    });
    // The callback runs while the device is polled, normally inside the first wait; the short receive covers the
    // moment between the GPU finishing and the callback being delivered, and the deadline covers everything else.
    let deadline = Instant::now() + WAIT_LIMIT;
    let mapping = loop {
        gpu.wait_idle(deadline.saturating_duration_since(Instant::now()))?;
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(RenderError::Backend(format!("the read-back was not delivered within {WAIT_LIMIT:?}")));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(RenderError::Backend("read-back channel closed before the mapping finished".into()));
            }
        }
    };
    mapping.map_err(|e| RenderError::Backend(format!("mapping the read-back buffer: {e}")))?;

    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .map_err(|e| RenderError::Backend(format!("reading the mapped buffer: {e}")))?;
    let mut rgba8 = Vec::with_capacity(row * size.y as usize);
    for line in mapped.chunks_exact(padded).take(size.y as usize) {
        rgba8.extend_from_slice(&line[..row]);
    }
    drop(mapped);
    buffer.unmap();
    Image::new(size, alpha, rgba8)
}
