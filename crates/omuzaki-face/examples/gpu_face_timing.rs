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
//! the renderer lends one through `PaintTarget::native`, and submitted.
//!
//! Every repaint follows a load: a compute pass that keeps the GPU busy for about 2 ms, submitted just before it. That
//! does two things. The GPU runs at the clocks of a busy frame (with nothing else to do it drops to its idle clock, and
//! every pass on it takes many times longer: that is its power policy, not the face, and the `face` bench of `omuzaki-test-stage` shows
//! it), and the repaint's commands are queued before the GPU reaches them, so no timestamp waits on the CPU (the old
//! probe's flaw: its timestamps, in separate submissions, measured the GPU waiting for Vello's CPU work).
//!
//! What is measured, per repaint:
//! - **GPU**, from timestamp queries: the rasterisation (Vello's compute stages, one pass), the resolve into the face
//!   texture (one render pass), and the total from the end of the load to the end of the resolve, which also holds the
//!   uploads (the packed scene and its config, a few kilobytes) and the bump buffer's clear.
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

#[path = "../tests/support/gpu_instance.rs"]
mod gpu_instance;

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
/// How long the load in front of each repaint keeps the GPU busy, in milliseconds: longer than the repaint's CPU work.
const LOAD_MS: f64 = 2.0;
/// The probe size's width in pixels.
const PROBE_WIDTH_PX: f32 = 256.0;
/// Repaints submitted before their timestamps are read back.
const BATCH: usize = 64;
/// Timestamps per repaint: the load's end, the raster pass's begin and end, the resolve pass's begin and end.
const STAMPS: usize = 5;
/// The load: a loop of integer work in one workgroup, as long as its uniform says.
const LOAD_WGSL: &str = "
@group(0) @binding(0) var<storage, read_write> sink: array<u32>;
@group(0) @binding(1) var<uniform> n: vec4<u32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    var x = id.x;
    for (var i = 0u; i < n.x; i++) { x = x * 1664525u + 1013904223u; }
    sink[id.x] = x;
}
";

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
}

/// One repaint's times in milliseconds.
#[derive(Copy, Clone)]
struct Times {
    raster: f64,
    resolve: f64,
    total: f64,
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
        }
    }

    /// The first query of repaint `slot` of the batch.
    fn base(slot: usize) -> u32 {
        u32::try_from(slot * STAMPS).unwrap_or(u32::MAX)
    }

    /// The raw timestamps of the first `queries` queries, in milliseconds, once their commands have run.
    fn read_ms(&self, device: &wgpu::Device, queue: &wgpu::Queue, queries: u32) -> Result<Vec<f64>, String> {
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
        let ms = {
            let mapped = self.read.slice(..).get_mapped_range().map_err(|e| format!("reading the timestamps: {e}"))?;
            // A few hundred million ticks at most, so the conversion is exact.
            #[allow(clippy::cast_precision_loss)]
            let at = |i: usize| u64::from_le_bytes(mapped[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8])) as f64;
            (0..queries as usize).map(|i| at(i) * self.period_ms).collect()
        };
        self.read.unmap();
        Ok(ms)
    }

    /// The times of the first `count` repaints of the batch.
    fn read(&self, device: &wgpu::Device, queue: &wgpu::Queue, count: usize) -> Result<Vec<Times>, String> {
        let ms = self.read_ms(device, queue, u32::try_from(count * STAMPS).map_err(|e| e.to_string())?)?;
        let span = |begin: f64, end: f64| (end - begin).max(0.0);
        Ok(ms
            .as_chunks::<STAMPS>()
            .0
            .iter()
            .map(|t| Times { raster: span(t[1], t[2]), resolve: span(t[3], t[4]), total: span(t[0], t[4]) })
            .collect())
    }
}

/// The compute pass that keeps the GPU busy in front of each repaint.
struct Load {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    iterations: wgpu::Buffer,
}

impl Load {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("load"),
            source: wgpu::ShaderSource::Wgsl(LOAD_WGSL.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("load"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let buffer = |size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("load"),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let sink = buffer(64 * 4, wgpu::BufferUsages::STORAGE);
        let iterations = buffer(16, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("load"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sink.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: iterations.as_entire_binding() },
            ],
        });
        Self { pipeline, bind, iterations }
    }

    /// Sets how many iterations the load runs.
    fn set(&self, queue: &wgpu::Queue, n: u32) {
        let mut bytes = [0_u8; 16];
        bytes[..4].copy_from_slice(&n.to_le_bytes());
        queue.write_buffer(&self.iterations, 0, &bytes);
    }

    /// Submits the load, writing its begin and end into `stamps` (`None` for either leaves it out).
    fn submit(&self, device: &wgpu::Device, queue: &wgpu::Queue, set: &wgpu::QuerySet, stamps: [Option<u32>; 2]) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("load") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("load"),
                timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                    query_set: set,
                    beginning_of_pass_write_index: stamps[0],
                    end_of_pass_write_index: stamps[1],
                }),
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);
    }

    /// Sizes the load to about [`LOAD_MS`] on this GPU.
    fn calibrate(&self, device: &wgpu::Device, queue: &wgpu::Queue, stamps: &Stamps) -> Result<(), String> {
        const TRIAL: u32 = 100_000;
        self.set(queue, TRIAL);
        let mut per_iteration = f64::INFINITY;
        // The fastest of a few runs: the first ones also wake the GPU.
        for _ in 0..8 {
            self.submit(device, queue, &stamps.set, [Some(0), Some(1)]);
            let t = stamps.read_ms(device, queue, 2)?;
            per_iteration = per_iteration.min((t[1] - t[0]) / f64::from(TRIAL));
        }
        if !(per_iteration.is_finite() && per_iteration > 0.0) {
            return Err("the GPU's timestamps did not move, so the probe can't time anything".to_owned());
        }
        // Thousands to millions of iterations: well inside a u32.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.set(queue, (LOAD_MS / per_iteration).clamp(1_000.0, 1e9) as u32);
        Ok(())
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

/// A device with timestamp queries and its backend.
fn device() -> Result<(wgpu::Device, wgpu::Queue, wgpu::Backend), String> {
    let instance = gpu_instance::instance();
    let options = wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..wgpu::RequestAdapterOptions::default()
    };
    let adapter =
        pollster::block_on(instance.request_adapter(&options)).map_err(|e| format!("no GPU adapter ({e})"))?;
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
    Ok((device, queue, adapter.get_info().backend))
}

/// One case's numbers over `frames` repaints, after the warm-up.
#[derive(Default)]
struct Case {
    raster: Vec<f64>,
    resolve: Vec<f64>,
    total: Vec<f64>,
    cpu: Vec<f64>,
    sections: usize,
}

/// What a case is timed with: the device, the timestamps and the load in front of each repaint.
struct Bench<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    stamps: &'a Stamps,
    load: &'a Load,
}

impl Bench<'_> {
    /// Times one case: `count` repaints after [`WARM_UP`], alternating `frames`.
    fn case(
        &self,
        spec: &FaceLayerSpec,
        frames: [&(FaceDrawing, LayerSections); 2],
        count: usize,
    ) -> Result<Case, String> {
        let (device, queue) = (self.device, self.queue);
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
                self.load.submit(device, queue, &self.stamps.set, [None, Some(base)]);
                rasterizer.time_passes(Some((self.stamps.set.clone(), base + 1)));
                let start = Instant::now();
                let drawn = rasterizer
                    .rasterize_native_sections(spec, &drawing[FaceLayerId::Base], sections, &mut native)
                    .map_err(|e| e.to_string())?;
                cpu.push(start.elapsed().as_secs_f64() * 1e3);
                if !drawn {
                    return Err("the rasteriser declined the device".to_owned());
                }
                case.sections = rasterizer.sections_encoded();
                // The renderer's part: submit what the painter recorded, with a fresh encoder for the next paint.
                let fresh = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                queue.submit([std::mem::replace(&mut native.3, fresh).finish()]);
            }
            let times = self.stamps.read(device, queue, batch)?;
            for (at, (t, cpu)) in times.iter().zip(cpu).enumerate() {
                if step + at >= WARM_UP {
                    case.raster.push(t.raster);
                    case.resolve.push(t.resolve);
                    case.total.push(t.total);
                    case.cpu.push(cpu);
                }
            }
            step += batch;
        }
        Ok(case)
    }
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
    let load = Load::new(&device);
    load.calibrate(&device, &queue, &stamps)?;
    let bench = Bench { device: &device, queue: &queue, stamps: &stamps, load: &load };

    println!(
        "Graphics backend: {backend:?}. Layer {width} x {height} px, {frames} repaints per case after {WARM_UP} warm-up, \
         each after a {LOAD_MS} ms GPU load. Times in ms, p50 / p95."
    );
    println!();
    println!(
        "| case | sections redone | GPU raster | GPU resolve | GPU total | CPU (encode + record) | budget ms | GPU p95 within budget |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---|");
    for (name, change) in CASES {
        let changed = frame(&mut painter, change)?;
        let mut case = bench.case(&spec, [&changed, &neutral], frames).map_err(|e| format!("{name}: {e}"))?;
        case.total.sort_by(f64::total_cmp);
        let within = percentile(&case.total, 95) <= BUDGET_MS;
        println!(
            "| {name} | {} | {} | {} | {} | {} | {BUDGET_MS} | {} |",
            case.sections,
            spread(&mut case.raster),
            spread(&mut case.resolve),
            spread(&mut case.total),
            spread(&mut case.cpu),
            if within { "yes" } else { "no" }
        );
    }
    Ok(())
}
