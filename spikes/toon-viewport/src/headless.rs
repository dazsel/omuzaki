//! Windowless rendering: one screenshot, or a frame-time benchmark.

use std::path::Path;
use std::time::Instant;

use crate::gpu::Gpu;
use crate::model::Model;
use crate::renderer::{FrameParams, RenderError, Renderer};
use crate::stats::Summary;

/// Why a headless run failed.
#[derive(Debug, thiserror::Error)]
pub enum HeadlessError {
    /// Rendering or readback failed.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// Writing the PNG failed.
    #[error("writing {path}: {source}")]
    Write { path: String, source: image::ImageError },
}

/// Writes tightly packed RGBA8 pixels to a PNG.
///
/// # Errors
/// Fails when the file cannot be written.
pub fn save_png(path: &Path, pixels: Vec<u8>, size: (u32, u32)) -> Result<(), HeadlessError> {
    let image = image::RgbaImage::from_raw(size.0, size.1, pixels)
        .ok_or(RenderError::Readback("pixel buffer size mismatch".to_owned()))?;
    image.save(path).map_err(|source| HeadlessError::Write { path: path.display().to_string(), source })
}

/// Renders one frame and writes it to `path`.
///
/// # Errors
/// Fails when the render or the write fails.
pub fn screenshot(
    gpu: &Gpu,
    model: &Model,
    params: &FrameParams<'_>,
    size: (u32, u32),
    path: &Path,
) -> Result<(), HeadlessError> {
    let mut renderer = Renderer::new(&gpu.device, &gpu.queue, model);
    renderer.render(size, params);
    let (pixels, size) = renderer.read_pixels()?;
    save_png(path, pixels, size)
}

/// What a benchmark measured.
#[derive(Clone, Copy, Debug)]
pub struct BenchReport {
    /// Submit to completion of one frame at a time (includes queue latency).
    pub serial: Summary,
    /// GPU time between the first and last pass of a frame, from timestamp queries, when supported.
    pub gpu: Option<Summary>,
    /// Frames submitted back to back with one wait at the end, as a sustained-throughput figure.
    pub pipelined_ms: f32,
}

/// Renders `frames` frames at `size` (after a warm-up) and reports their times.
pub fn bench(gpu: &Gpu, model: &Model, params: &FrameParams<'_>, size: (u32, u32), frames: u32) -> BenchReport {
    const WARMUP: u32 = 30;
    let mut renderer = Renderer::new(&gpu.device, &gpu.queue, model);
    for _ in 0..WARMUP {
        renderer.render(size, params);
        renderer.finish_timing();
    }
    let (mut serial, mut gpu_times) = (Vec::new(), Vec::new());
    for _ in 0..frames {
        let start = Instant::now();
        renderer.render(size, params);
        let gpu_ms = renderer.finish_timing();
        serial.push(start.elapsed().as_secs_f32() * 1000.0);
        gpu_times.extend(gpu_ms);
    }
    let start = Instant::now();
    for _ in 0..frames {
        renderer.render(size, params);
    }
    renderer.finish_timing();
    let pipelined_ms =
        start.elapsed().as_secs_f32() * 1000.0 / f32::from(u16::try_from(frames.max(1)).unwrap_or(u16::MAX));
    BenchReport {
        serial: Summary::of(&serial).unwrap_or(Summary { count: 0, mean: 0.0, p50: 0.0, p95: 0.0, max: 0.0 }),
        gpu: Summary::of(&gpu_times),
        pipelined_ms,
    }
}
