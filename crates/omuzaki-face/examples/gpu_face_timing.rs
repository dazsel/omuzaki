//! The timing probe for the GPU face rasteriser (#1433 B1, run as #1658, profiled for #2074): how long a repaint of the
//! face layer takes on the GPU, against the 0.5 ms face-paint budget (spec v3 §10), for four changes: a full face, a
//! mouth-only change, eyes + mouth + brows, and brows only (#1392).
//!
//! ```text
//! cargo run --release -p omuzaki-face --example gpu_face_timing -- [--frames N] [--probe]
//! ```
//!
//! `--frames` is how many repaints each case is timed over (2,000 by default); `--probe` uses the 256 x 160 probe size
//! instead of the layer's real size. A repaint alternates the neutral face with the case's face, through the sections path
//! (`VelloRasterizer`), so a mouth-only change repaints only the mouth. Each repaint is recorded into its own encoder, as
//! the renderer lends one through `PaintTarget::native`, and submitted back to back with the others, so the GPU is busy as
//! it is in a live frame.
//!
//! What is measured, per repaint:
//! - **GPU**, from timestamp queries: the rasterisation (Vello's compute stages, one pass), the resolve into the face
//!   texture (one render pass), and the whole of the face's commands between two timestamps written on the encoder. The
//!   uploads (the packed scene and its config, a few kilobytes) are copied just before the encoder runs and are not in it.
//! - **CPU**, on the calling thread: the rasteriser's call, which fingerprints the sections, encodes the changed ones,
//!   packs the scene and records the passes. Submitting is the renderer's and is not in it.
//!
//! Without timestamp queries the probe exits with 1. The output is a fixed markdown table that can be pasted as it is,
//! and the graphics backend. It never prints the adapter's name or any other hardware detail: the repository is public.
//! On a software adapter (lavapipe) the numbers check that the probe runs; they are not a timing (decision 7.3).

// The GPU rasteriser exists on native targets only (#2207).
#![cfg(not(target_arch = "wasm32"))]

use std::process::ExitCode;
use std::time::Instant;

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
/// Repaints submitted before their timestamps are read back.
const BATCH: usize = 64;
/// Timestamps per repaint: the encoder's begin, the raster pass's two, the resolve pass's two, the encoder's end.
const STAMPS: usize = 6;

/// What a case changes in the resting parameters.
type Change = fn(&mut PaintParams);

/// The cases: a name and its change.
const CASES: [(&str, Change); 4] = [
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
fn frame(painter: &mut RealPainter, change: Change) -> Result<(FaceDrawing, LayerSections), String> {
    let profile = mock_profile();
    let mut params = FaceMapping::default().rest_params();
    change(&mut params);
    let mut drawing = FaceDrawing::new();
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint(&input, &mut drawing).map_err(|e| format!("the painter failed: {e}"))?;
    Ok((drawing, painter.sections().layer(FaceLayerId::Base).clone()))
}

/// The timestamps of a batch of repaints and where they are read back.
struct Stamps {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    period_ms: f64,
    /// Whether timestamps can be written on an encoder, between passes (the whole span); without it the span is the sum
    /// of the two passes.
    inside_encoders: bool,
}

/// One repaint's times in milliseconds.
#[derive(Copy, Clone)]
struct Times {
    raster: f64,
    resolve: f64,
    span: f64,
}

impl Stamps {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let bytes = (BATCH * STAMPS * 8) as u64;
        let buffer = |usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timing"),
                size: bytes,
                usage,
                mapped_at_creation: false,
            })
        };
        Self {
            set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("timing"),
                ty: wgpu::QueryType::Timestamp,
                count: u32::try_from(BATCH * STAMPS).unwrap_or(u32::MAX),
            }),
            resolve: buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC),
            read: buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ),
            period_ms: f64::from(queue.get_timestamp_period()) / 1.0e6,
            inside_encoders: device.features().contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
        }
    }

    /// The first query of repaint `slot` of the batch.
    fn base(slot: usize) -> u32 {
        u32::try_from(slot * STAMPS).unwrap_or(u32::MAX)
    }

    /// Reads back the first `count` repaints of the batch, once their commands have run.
    fn read(&self, device: &wgpu::Device, queue: &wgpu::Queue, count: usize) -> Result<Vec<Times>, String> {
        let queries = u32::try_from(count * STAMPS).map_err(|e| e.to_string())?;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("timing") });
        encoder.resolve_query_set(&self.set, 0..queries, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, u64::from(queries) * 8);
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        self.read.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            // The receiver outlives the wait below; a failed send only means the reader gave up.
            let _ = sender.send(result);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| format!("the device did not finish: {e}"))?;
        receiver.recv().map_err(|e| e.to_string())?.map_err(|e| format!("mapping the timestamps: {e}"))?;
        let times = {
            let mapped = self.read.slice(..).get_mapped_range().map_err(|e| format!("reading the timestamps: {e}"))?;
            let at = |i: usize| u64::from_le_bytes(mapped[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
            #[allow(clippy::cast_precision_loss)] // a few hundred million ticks at most
            let ms = |begin: usize, end: usize| at(end).saturating_sub(at(begin)) as f64 * self.period_ms;
            (0..count)
                .map(|slot| {
                    let b = slot * STAMPS;
                    let (raster, resolve) = (ms(b + 1, b + 2), ms(b + 3, b + 4));
                    let span = if self.inside_encoders { ms(b, b + 5) } else { raster + resolve };
                    Times { raster, resolve, span }
                })
                .collect()
        };
        self.read.unmap();
        Ok(times)
    }
}

/// `p`-th percentile of sorted `values`.
fn percentile(values: &[f64], p: usize) -> f64 {
    values[(values.len() * p / 100).min(values.len() - 1)]
}

/// `values` as "p50 / p95", sorted in place.
fn spread(values: &mut [f64]) -> String {
    values.sort_by(f64::total_cmp);
    format!("{:.3} / {:.3}", percentile(values, 50), percentile(values, 95))
}

/// The layer's texture, as the renderer makes it for a face layer.
fn face_texture(device: &wgpu::Device, (width, height): (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
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
    })
}

/// A device with timestamp queries (inside encoders too, if the adapter has them) and its backend.
fn device() -> Result<(wgpu::Device, wgpu::Queue, wgpu::Backend), String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let options = wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..wgpu::RequestAdapterOptions::default()
    };
    let adapter =
        pollster::block_on(instance.request_adapter(&options)).map_err(|e| format!("no GPU adapter ({e})"))?;
    if !adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
        return Err("this adapter has no timestamp queries, so the GPU time can't be measured".to_owned());
    }
    let features =
        adapter.features() & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
    let descriptor = wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: adapter.limits(),
        ..wgpu::DeviceDescriptor::default()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
        .map_err(|e| format!("the adapter gave no device ({e})"))?;
    Ok((device, queue, adapter.get_info().backend))
}

/// One case's numbers over `frames` repaints, after the warm-up.
#[derive(Default)]
struct Case {
    raster: Vec<f64>,
    resolve: Vec<f64>,
    span: Vec<f64>,
    cpu: Vec<f64>,
    sections: usize,
}

/// Times one case: `frames` repaints alternating the neutral face with `changed`.
fn time_case(
    gpu: (&wgpu::Device, &wgpu::Queue),
    stamps: &Stamps,
    spec: &FaceLayerSpec,
    frames: [&(FaceDrawing, LayerSections); 2],
    count: usize,
) -> Result<Case, String> {
    let (device, queue) = gpu;
    let mut native = (
        device.clone(),
        queue.clone(),
        face_texture(device, spec.size_px()),
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default()),
    );
    let mut rasterizer = VelloRasterizer::new();
    let mut case = Case::default();
    let mut step = 0;
    while step < WARM_UP + count {
        let batch = BATCH.min(WARM_UP + count - step);
        let mut cpu = Vec::with_capacity(batch);
        for slot in 0..batch {
            let (drawing, sections) = frames[(step + slot) % 2];
            let base = Stamps::base(slot);
            if stamps.inside_encoders {
                native.3.write_timestamp(&stamps.set, base);
            }
            rasterizer.time_passes(Some((stamps.set.clone(), base + 1)));
            let start = Instant::now();
            let drawn = rasterizer
                .rasterize_native_sections(spec, &drawing[FaceLayerId::Base], sections, &mut native)
                .map_err(|e| e.to_string())?;
            cpu.push(start.elapsed().as_secs_f64() * 1e3);
            if !drawn {
                return Err("the rasteriser declined the device".to_owned());
            }
            case.sections = rasterizer.sections_encoded();
            if stamps.inside_encoders {
                native.3.write_timestamp(&stamps.set, base + 5);
            }
            // The renderer's part: submit what the painter recorded, with a fresh encoder for the next paint.
            let fresh = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            queue.submit([std::mem::replace(&mut native.3, fresh).finish()]);
        }
        let times = stamps.read(device, queue, batch)?;
        for (slot, (t, cpu)) in times.iter().zip(cpu).enumerate() {
            if step + slot >= WARM_UP {
                case.raster.push(t.raster);
                case.resolve.push(t.resolve);
                case.span.push(t.span);
                case.cpu.push(cpu);
            }
        }
        step += batch;
    }
    Ok(case)
}

fn run() -> Result<(), String> {
    let (frames, probe) = arguments()?;
    let (device, queue, backend) = device()?;
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let neutral = frame(&mut painter, |_| {})?;
    let rect = FaceLayerSpec::for_layer(FaceLayerId::Base, &mock_projection(), DEFAULT_PX_PER_M).rect;
    let spec = if probe {
        FaceLayerSpec::for_rect(rect, FaceLayerId::Base, PROBE_WIDTH_PX / rect.width())
    } else {
        FaceLayerSpec::for_rect(rect, FaceLayerId::Base, DEFAULT_PX_PER_M)
    };
    let (width, height) = spec.size_px();
    let stamps = Stamps::new(&device, &queue);

    println!(
        "Graphics backend: {backend:?}. Layer {width} x {height} px, {frames} repaints per case after {WARM_UP} warm-up, \
         submitted back to back. Times in ms, p50 / p95.{}",
        if stamps.inside_encoders { "" } else { " No timestamps inside encoders: the GPU total is raster + resolve." }
    );
    println!();
    println!(
        "| case | sections redone | GPU raster | GPU resolve | GPU total (face commands) | CPU (encode + record) | budget ms | GPU p95 within budget |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---|");
    for (name, change) in CASES {
        let changed = frame(&mut painter, change)?;
        let mut case = time_case((&device, &queue), &stamps, &spec, [&changed, &neutral], frames)
            .map_err(|e| format!("{name}: {e}"))?;
        case.span.sort_by(f64::total_cmp);
        let within = percentile(&case.span, 95) <= BUDGET_MS;
        println!(
            "| {name} | {} | {} | {} | {} | {} | {BUDGET_MS} | {} |",
            case.sections,
            spread(&mut case.raster),
            spread(&mut case.resolve),
            spread(&mut case.span),
            spread(&mut case.cpu),
            if within { "yes" } else { "no" }
        );
    }
    Ok(())
}
