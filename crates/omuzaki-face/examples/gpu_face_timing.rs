//! The timing probe for the GPU face rasteriser (#1433 B1, run by the owner as #1658): how long a repaint of the face layer
//! takes on the GPU, against the 0.5 ms face-paint budget (spec v3 §10), for four changes: a full face, a mouth-only change,
//! eyes + mouth + brows, and brows only (#1392).
//!
//! ```text
//! cargo run --release -p omuzaki-face --example gpu_face_timing -- [--frames N] [--probe]
//! ```
//!
//! `--frames` is how many repaints each case is timed over (2,000 by default); `--probe` uses the 256 x 160 probe size
//! instead of the layer's real size. A repaint alternates the neutral face with the case's face, through the sections path
//! (`VelloRasterizer`), so a mouth-only change repaints only the mouth. The time is from GPU timestamps taken before and
//! after the repaint on the same queue, so it covers Vello's passes and the resolve, not the CPU encode. Without timestamp
//! queries the probe exits with 1.
//!
//! The output is a fixed markdown table that can be pasted as it is, and the graphics backend. It never prints the adapter's
//! name or any other hardware detail: the repository is public. On a software adapter (lavapipe) the numbers check that
//! the probe runs; they are not a timing (decision 7.3).

use std::process::ExitCode;

use omuzaki_core::time::Seconds;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{GpuRasterizer, LayerSections, RealPainter};
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping, FacePainter, FaceView,
    PaintParams,
};
use omuzaki_face_mock::{mock_profile, mock_projection};

/// The face-paint budget in milliseconds (spec v3 §10).
const BUDGET_MS: f64 = 0.5;
const DEFAULT_FRAMES: usize = 2000;
const WARM_UP: usize = 50;
/// The probe size's width in pixels.
const PROBE_WIDTH_PX: f32 = 256.0;

/// A case: its name and what it changes in the resting parameters.
const CASES: [(&str, fn(&mut PaintParams)); 4] = [
    ("full face", |p| {
        p.mouth.open = 0.6;
        p.mouth.smile_left = 1.0;
        p.left_eye.close = 0.6;
        p.blush = 1.0;
        p.gaze = [0.5, 0.3].into();
        p.left_brow.height = 1.0;
    }),
    ("mouth only", |p| p.mouth.open = 0.6),
    ("eyes + mouth + brows", |p| {
        p.mouth.open = 0.6;
        p.left_eye.close = 0.6;
        p.left_brow.height = 1.0;
    }),
    ("brows only", |p| {
        p.left_brow.height = 1.0;
        p.right_brow.height = 1.0;
    }),
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("gpu_face_timing: {message}");
            ExitCode::FAILURE
        }
    }
}

/// `--frames N` and `--probe` from the command line.
fn arguments() -> Result<(usize, bool), String> {
    let (mut frames, mut probe) = (DEFAULT_FRAMES, false);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--probe" => probe = true,
            "--frames" => {
                let value = args.next().ok_or("--frames needs a number")?;
                frames = value.parse().map_err(|_| format!("--frames {value:?} is not a number"))?;
                if frames == 0 {
                    return Err("--frames must be at least 1".to_owned());
                }
            }
            other => return Err(format!("unknown argument {other:?}; use --frames N and --probe")),
        }
    }
    Ok((frames, probe))
}

/// One frame: its drawing and the section ranges of its Base layer.
fn frame(painter: &mut RealPainter, change: fn(&mut PaintParams)) -> Result<(FaceDrawing, LayerSections), String> {
    let profile = mock_profile();
    let mut params = FaceMapping::default().rest_params();
    change(&mut params);
    let mut drawing = FaceDrawing::new();
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint(&input, &mut drawing).map_err(|e| format!("the painter failed: {e}"))?;
    Ok((drawing, painter.sections().layer(FaceLayerId::Base).clone()))
}

/// The two GPU timestamps around a repaint.
struct Stamps {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    period_ms: f64,
}

impl Stamps {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let buffer = |usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timing"),
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        Self {
            set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("timing"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            }),
            resolve: buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC),
            read: buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ),
            period_ms: f64::from(queue.get_timestamp_period()) / 1.0e6,
        }
    }

    /// Submits an empty compute pass that writes timestamp `index`.
    fn stamp(&self, device: &wgpu::Device, queue: &wgpu::Queue, index: u32) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("timing") });
        let writes = if index == 0 {
            wgpu::PassTimestampWrites {
                query_set: &self.set,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: None,
            }
        } else {
            wgpu::PassTimestampWrites {
                query_set: &self.set,
                beginning_of_pass_write_index: None,
                end_of_pass_write_index: Some(1),
            }
        };
        drop(encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("timing"),
            timestamp_writes: Some(writes),
        }));
        queue.submit([encoder.finish()]);
    }

    /// Milliseconds between the two timestamps, once both have been written.
    fn elapsed_ms(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<f64, String> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("timing") });
        encoder.resolve_query_set(&self.set, 0..2, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, 16);
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        self.read.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            // The receiver outlives the wait below; a failed send only means the reader gave up.
            let _ = sender.send(result);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| format!("the device did not finish: {e}"))?;
        receiver.recv().map_err(|e| e.to_string())?.map_err(|e| format!("mapping the timestamps: {e}"))?;
        let ticks = {
            let mapped = self.read.slice(..).get_mapped_range().map_err(|e| format!("reading the timestamps: {e}"))?;
            let at = |i: usize| u64::from_le_bytes(mapped[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
            at(1).saturating_sub(at(0))
        };
        self.read.unmap();
        #[allow(clippy::cast_precision_loss)] // a few hundred million ticks at most
        Ok(ticks as f64 * self.period_ms)
    }
}

/// `p`-th percentile of sorted `values`.
fn percentile(values: &[f64], p: usize) -> f64 {
    values[(values.len() * p / 100).min(values.len() - 1)]
}

fn run() -> Result<(), String> {
    let (frames, probe) = arguments()?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .map_err(|e| format!("no GPU adapter ({e})"))?;
    if !adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
        return Err("this adapter has no timestamp queries, so the GPU time can't be measured".to_owned());
    }
    let descriptor = wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TIMESTAMP_QUERY,
        required_limits: adapter.limits(),
        ..wgpu::DeviceDescriptor::default()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
        .map_err(|e| format!("the adapter gave no device ({e})"))?;
    let backend = adapter.get_info().backend;

    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let neutral = frame(&mut painter, |_| {})?;
    let rect = FaceLayerSpec::for_layer(FaceLayerId::Base, &mock_projection(), DEFAULT_PX_PER_M).rect;
    let spec = if probe {
        FaceLayerSpec::for_rect(rect, FaceLayerId::Base, PROBE_WIDTH_PX / rect.width())
    } else {
        FaceLayerSpec::for_rect(rect, FaceLayerId::Base, DEFAULT_PX_PER_M)
    };
    let (width, height) = spec.size_px();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("face layer"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut native = (device.clone(), queue.clone(), texture);
    let stamps = Stamps::new(&device, &queue);

    println!(
        "Graphics backend: {backend:?}. Layer {width} x {height} px, {frames} frames per case after {WARM_UP} warm-up."
    );
    println!();
    println!("| case | frames | p50 ms | p95 ms | budget ms | within budget |");
    println!("|---|---:|---:|---:|---:|---|");
    for (name, change) in CASES {
        let changed = frame(&mut painter, change)?;
        let mut rasterizer = VelloRasterizer::new();
        let mut times = Vec::with_capacity(frames);
        for step in 0..WARM_UP + frames {
            let (drawing, sections) = if step % 2 == 0 { &changed } else { &neutral };
            stamps.stamp(&device, &queue, 0);
            let drawn = rasterizer
                .rasterize_native_sections(&spec, &drawing[FaceLayerId::Base], sections, &mut native)
                .map_err(|e| format!("{name}: {e}"))?;
            if !drawn {
                return Err(format!("{name}: the rasteriser declined the device"));
            }
            stamps.stamp(&device, &queue, 1);
            let ms = stamps.elapsed_ms(&device, &queue)?;
            if step >= WARM_UP {
                times.push(ms);
            }
        }
        times.sort_by(f64::total_cmp);
        let (p50, p95) = (percentile(&times, 50), percentile(&times, 95));
        println!(
            "| {name} | {frames} | {p50:.3} | {p95:.3} | {BUDGET_MS} | {} |",
            if p95 <= BUDGET_MS { "yes" } else { "no" }
        );
    }
    Ok(())
}
