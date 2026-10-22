//! The timing probe for the GPU face rasteriser (#1433 B1, run as #1658, profiled for #2074, gated by #318): how long a
//! repaint of the face takes on the GPU, against the 0.5 ms face-paint budget (spec v3 §10, `gpu.face_paint`), for five
//! changes: the worst case (every section of every layer, with every effect), a full face, a mouth-only change, eyes +
//! mouth + brows, and brows only (#1392).
//!
//! ```text
//! cargo run --release -p omuzaki-test-face --example gpu_face_timing -- [--frames N] [--probe] [--report DIR]
//!     [--class reported|measured|scaled|relative] [--previous report.json] [--strict] [--paced FPS]
//! ```
//!
//! `--frames` is how many repaints each case is timed over (2,000 by default); `--probe` uses the 256 px wide probe size
//! instead of the layers' real size. A repaint alternates the neutral face with the case's face, through the sections
//! path (`VelloRasterizer`, one for all layers as the face system keeps it), so a mouth-only change repaints only the
//! mouth. Each repaint is recorded into the encoders the renderer lends through `PaintTarget::native`, one per layer
//! texture, and submitted together.
//!
//! Every repaint follows a load: a compute pass that keeps the GPU busy for about 2 ms, submitted just before it. That
//! does two things. The GPU runs at the clocks of a busy frame (with nothing else to do it drops to its idle clock, and
//! every pass on it takes many times longer: that is its power policy, not the face, and the `face` bench of
//! `omuzaki-test-stage` shows it), and the repaint's commands are queued before the GPU reaches them, so no timestamp
//! waits on the CPU (the old probe's flaw: its timestamps, in separate submissions, measured the GPU waiting for Vello's
//! CPU work).
//!
//! What is measured, per repaint:
//! - **GPU**, from timestamp queries: the rasterisation (Vello's compute stages, one pass per layer), the resolve into
//!   the layer textures (one render pass per layer), and the total from the end of the load to the end of the last
//!   resolve, which also holds the uploads (the packed scenes and their configs, a few kilobytes) and the bump buffers'
//!   clears.
//! - **CPU**, on the calling thread: the rasteriser's calls, which fingerprint the sections, encode the changed ones,
//!   pack the scenes and record the passes. Submitting is the renderer's and is not in it.
//!
//! **The paced reading.** `--paced FPS` then times every case again as the live loop paints: no load, each repaint alone,
//! `1/FPS` apart, after a 20 s warm-up (spec §10, "Paced, warm, long enough"). The GPU then idles between repaints,
//! and the repaint is the first work it meets after the idle gap, as the face paint is in a live frame. Its rows are
//! `<key>.paced`, reported beside the steady ones and never judged: §10 holds the steady reading against the budget,
//! and the paced one shows what the GPU's clock state adds to the same work. The repaint's commands are recorded
//! before a marker's are submitted with them, so here too no timestamp waits on the CPU.
//!
//! **The budget report.** The worst case's GPU total is `gpu.face_paint`, judged against the §10 table in the budget
//! harness's report (#147); the other cases are reported beside it under `gpu.face_paint.<case>`, and the CPU side
//! under `cpu.face_paint`. `--report DIR` writes `report.md` and `report.json` there, as `tools/ci/budget-report.sh`
//! does. `--class` says what the numbers are (spec §10, "Numbers from other hardware"): `reported` by default, which
//! never gates; `measured` and `scaled` need the provenance variables (`OMUZAKI_BUDGET_TIER`, `_OS_BUILD`, `_GPU`,
//! `_BACKEND`, and `_SCALING_RULE` for `scaled`). `--strict` exits with 1 on what the class gates on: `--class measured
//! --strict` on the owner's GPU machine is the 0.5 ms gate (#1658). A software adapter (lavapipe) is never a timing
//! (decision 7.3): its numbers check that the probe runs, and it refuses any class but `reported`.
//!
//! Without timestamp queries the probe exits with 1. The output is a fixed markdown table that can be pasted as it is,
//! and the graphics backend. It never prints the adapter's name or any other hardware detail: the repository is public.

// The GPU rasteriser exists on native targets only (#2207).
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[path = "../../omuzaki-face/tests/support/gpu_instance.rs"]
mod gpu_instance;

use omuzaki_core::time::Seconds;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{GpuRasterizer, LayerSections, RealPainter};
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping, FacePainter, FaceView,
    PaintParams,
};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_test::budget::report::{Class, Machine, Metric, Provenance, Report};
use omuzaki_test::budget::table::parse_spec;

/// The face-paint budget in milliseconds (spec v3 §10).
const BUDGET_MS: f64 = 0.5;
/// The §10 row the worst case is judged against.
const BUDGET_KEY: &str = "gpu.face_paint";
/// The spec holding the §10 table, built in so the probe runs where the repository isn't (#1658's GPU machine).
const SPEC: &str = include_str!("../../../docs/product/02-studio-spec-v3.md");
const DEFAULT_FRAMES: usize = 2000;
const WARM_UP: usize = 50;
/// The paced reading's warm-up, in seconds (spec §10).
const PACED_WARM_UP_S: u32 = 20;
/// How long the load in front of each repaint keeps the GPU busy, in milliseconds: longer than the repaint's CPU work.
const LOAD_MS: f64 = 2.0;
/// The probe size's width in pixels.
const PROBE_WIDTH_PX: f32 = 256.0;
/// Repaints submitted before their timestamps are read back.
const BATCH: usize = 64;
/// Timestamps per layer: the raster pass's begin and end, the resolve pass's begin and end.
const LAYER_STAMPS: usize = 4;
/// Timestamps per repaint: the load's end, then each layer's.
const STAMPS: usize = 1 + LAYER_STAMPS * FaceLayerId::COUNT;
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

/// One case: its name, its key in the report, the layers it repaints and its change.
struct CaseSpec {
    name: &'static str,
    key: &'static str,
    layers: &'static [FaceLayerId],
    change: Change,
}

/// The cases. The worst case comes first: it is the one judged against the budget.
const CASES: [CaseSpec; 5] = [
    CaseSpec { name: "worst case", key: BUDGET_KEY, layers: &FaceLayerId::ALL, change: worst },
    CaseSpec {
        name: "full face",
        key: "gpu.face_paint.full_face",
        layers: &[FaceLayerId::Base],
        change: |p| {
            p.mouth.open = 0.6;
            p.mouth.smile_left = 1.0;
            p.left_eye.close = 0.6;
            p.blush = 1.0;
            p.gaze = [0.5, 0.3].into();
            p.left_brow.height = 1.0;
        },
    },
    CaseSpec {
        name: "mouth only",
        key: "gpu.face_paint.mouth",
        layers: &[FaceLayerId::Base],
        change: |p| p.mouth.open = 0.6,
    },
    CaseSpec {
        name: "eyes + mouth + brows",
        key: "gpu.face_paint.eyes_mouth_brows",
        layers: &[FaceLayerId::Base],
        change: |p| {
            p.mouth.open = 0.6;
            p.left_eye.close = 0.6;
            p.left_brow.height = 1.0;
        },
    },
    CaseSpec {
        name: "brows only",
        key: "gpu.face_paint.brows",
        layers: &[FaceLayerId::Base],
        change: |p| {
            p.left_brow.height = 1.0;
            p.right_brow.height = 1.0;
        },
    },
];

/// The worst case: every section of every layer changes, and every effect that adds drawing shows (tears, sparkles,
/// hearts, gloom, sweat, a full blush, an open mouth with teeth, tongue and fangs).
fn worst(p: &mut PaintParams) {
    for eye in [&mut p.left_eye, &mut p.right_eye] {
        eye.open = 1.0;
        eye.close = 0.2;
        eye.lower_lid = 0.3;
    }
    p.eye_tilt = 0.8;
    p.pupil = 1.3;
    p.gaze = [0.5, 0.3].into();
    p.vergence = 0.2;
    p.effects.heart = 1.0;
    p.effects.sparkle = 1.0;
    p.effects.gloom = 1.0;
    p.effects.tears = 1.0;
    for brow in [&mut p.left_brow, &mut p.right_brow] {
        brow.height = 1.0;
        brow.tilt = 0.5;
    }
    p.brow_arch = 0.6;
    p.mouth.open = 0.8;
    p.mouth.wide = 0.5;
    p.mouth.smile_left = 1.0;
    p.mouth.smile_right = 0.8;
    p.mouth.teeth_upper = 1.0;
    p.mouth.teeth_lower = 1.0;
    p.mouth.tongue = 1.0;
    p.mouth.fangs = 1.0;
    p.blush = 1.6;
    p.cheek_puff = 0.5;
    p.sweat = 1.0;
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("gpu_face_timing: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The command line.
struct Arguments {
    frames: usize,
    probe: bool,
    /// The folder `report.md` and `report.json` go to.
    report: Option<PathBuf>,
    class: Class,
    previous: Option<PathBuf>,
    strict: bool,
    /// The frame rate of the paced reading, if one is asked for.
    paced: Option<u32>,
}

/// The command line, from `std::env::args`.
fn arguments() -> Result<Arguments, String> {
    let mut parsed = Arguments {
        frames: DEFAULT_FRAMES,
        probe: false,
        report: None,
        class: Class::Reported,
        previous: None,
        strict: false,
        paced: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--probe" => parsed.probe = true,
            "--strict" => parsed.strict = true,
            "--report" => parsed.report = Some(PathBuf::from(value()?)),
            "--previous" => parsed.previous = Some(PathBuf::from(value()?)),
            "--class" => parsed.class = Class::parse(&value()?).map_err(|e| e.to_string())?,
            "--paced" => {
                let value = value()?;
                let fps: u32 = value.parse().map_err(|_| format!("--paced {value:?} is not a frame rate"))?;
                if !(1..=240).contains(&fps) {
                    return Err("--paced takes a frame rate from 1 to 240".to_owned());
                }
                parsed.paced = Some(fps);
            }
            "--frames" => {
                let value = value()?;
                parsed.frames = value.parse().map_err(|_| format!("--frames {value:?} is not a number"))?;
                if parsed.frames == 0 {
                    return Err("--frames must be at least 1".to_owned());
                }
            }
            other => {
                return Err(format!(
                    "unknown argument {other:?}; use --frames N, --probe, --report DIR, --class CLASS, --previous FILE \
                     --strict and --paced FPS"
                ));
            }
        }
    }
    Ok(parsed)
}

/// One frame of every layer: the drawing and the section ranges of each layer.
struct Frame {
    drawing: FaceDrawing,
    sections: [LayerSections; FaceLayerId::COUNT],
}

fn frame(painter: &mut RealPainter, change: Change) -> Result<Frame, String> {
    let profile = mock_profile();
    let mut params = FaceMapping::default().rest_params();
    change(&mut params);
    let mut drawing = FaceDrawing::new();
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint(&input, &mut drawing).map_err(|e| format!("the painter failed: {e}"))?;
    let sections = FaceLayerId::ALL.map(|id| painter.sections().layer(id).clone());
    Ok(Frame { drawing, sections })
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

    /// The times of the first `count` repaints of the batch, each of which painted its first `layers` layer slots.
    fn read(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        count: usize,
        layers: usize,
    ) -> Result<Vec<Times>, String> {
        let ms = self.read_ms(device, queue, u32::try_from(count * STAMPS).map_err(|e| e.to_string())?)?;
        let span = |begin: f64, end: f64| (end - begin).max(0.0);
        Ok(ms
            .as_chunks::<STAMPS>()
            .0
            .iter()
            .map(|t| {
                let layer = |i: usize| &t[1 + i * LAYER_STAMPS..][..LAYER_STAMPS];
                Times {
                    raster: (0..layers).map(|i| span(layer(i)[0], layer(i)[1])).sum(),
                    resolve: (0..layers).map(|i| span(layer(i)[2], layer(i)[3])).sum(),
                    total: span(t[0], layer(layers - 1)[3]),
                }
            })
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

/// An empty compute pass that writes timestamp `end`: the paced reading's start, submitted with the repaint it starts.
fn marker(device: &wgpu::Device, set: &wgpu::QuerySet, end: u32) -> wgpu::CommandBuffer {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("marker") });
    drop(encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("marker"),
        timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
            query_set: set,
            beginning_of_pass_write_index: None,
            end_of_pass_write_index: Some(end),
        }),
    }));
    encoder.finish()
}

/// Waits for the next tick of a clock that ticks every `interval`, from `next`; a late tick starts the clock again.
fn wait(next: &mut Instant, interval: Duration) {
    *next += interval;
    let now = Instant::now();
    if *next > now {
        std::thread::sleep(*next - now);
    } else {
        *next = now;
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

/// A device with timestamp queries, its backend and whether it is a software adapter.
fn device() -> Result<(wgpu::Device, wgpu::Queue, wgpu::Backend, bool), String> {
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
    let info = adapter.get_info();
    Ok((device, queue, info.backend, info.device_type == wgpu::DeviceType::Cpu))
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

/// What a case is timed with: the device, the timestamps and the load in front of each repaint, or the pace.
struct Bench<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    stamps: &'a Stamps,
    load: &'a Load,
    /// The time between paced repaints; `None` for the steady reading (the load in front of each, no pause).
    pace: Option<Duration>,
}

/// What the renderer lends a face painter through `PaintTarget::native`: the device, the queue, the layer's texture and
/// the encoder to record into.
type Native = (wgpu::Device, wgpu::Queue, wgpu::Texture, wgpu::CommandEncoder);

impl Bench<'_> {
    /// Times one case: `count` repaints of `specs` (one per layer it repaints) after the warm-up, alternating `frames`.
    fn case(&self, specs: &[FaceLayerSpec], frames: [&Frame; 2], count: usize) -> Result<Case, String> {
        let warm_up = self.pace.map_or(WARM_UP, |interval| {
            // Whole frames, a few thousand at most.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let frames = (f64::from(PACED_WARM_UP_S) / interval.as_secs_f64()).ceil() as usize;
            frames
        });
        let mut next = Instant::now();
        let (device, queue) = (self.device, self.queue);
        let encoder = || device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let mut natives: Vec<Native> = specs
            .iter()
            .map(|spec| (device.clone(), queue.clone(), face_texture(device, spec.size_px()), encoder()))
            .collect();
        let mut rasterizer = VelloRasterizer::new();
        let mut case = Case::default();
        let mut step = 0;
        while step < warm_up + count {
            let batch = BATCH.min(warm_up + count - step);
            let mut cpu = Vec::with_capacity(batch);
            for slot in 0..batch {
                let frame = frames[(step + slot) % 2];
                let base = Stamps::base(slot);
                // Steady: the load runs while the repaint is recorded. Paced: a marker goes in with the repaint.
                let start = match self.pace {
                    None => {
                        self.load.submit(device, queue, &self.stamps.set, [None, Some(base)]);
                        None
                    }
                    Some(_) => Some(marker(device, &self.stamps.set, base)),
                };
                let (mut elapsed, mut sections) = (0.0, 0);
                for (at, (spec, native)) in specs.iter().zip(&mut natives).enumerate() {
                    let first = base + 1 + u32::try_from(at * LAYER_STAMPS).map_err(|e| e.to_string())?;
                    rasterizer.time_passes(Some((self.stamps.set.clone(), first)));
                    let (list, layer_sections) = (&frame.drawing[spec.id], &frame.sections[spec.id as usize]);
                    let start = Instant::now();
                    let drawn = rasterizer
                        .rasterize_native_sections(spec, list, layer_sections, native)
                        .map_err(|e| e.to_string())?;
                    elapsed += start.elapsed().as_secs_f64() * 1e3;
                    if !drawn {
                        return Err("the rasteriser declined the device".to_owned());
                    }
                    // A layer that didn't change writes no timestamps, and its slots would be read as garbage.
                    if rasterizer.last_damage().is_none_or(|d| d.width == 0 || d.height == 0) {
                        return Err(format!("the {:?} layer did not change between the case's frames", spec.id));
                    }
                    sections += rasterizer.sections_encoded();
                }
                cpu.push(elapsed);
                case.sections = sections;
                // The renderer's part: submit what the painter recorded, with fresh encoders for the next paint.
                let painted = natives.iter_mut().map(|n| std::mem::replace(&mut n.3, encoder()).finish());
                queue.submit(start.into_iter().chain(painted));
                if let Some(interval) = self.pace {
                    wait(&mut next, interval);
                }
            }
            let times = self.stamps.read(device, queue, batch, specs.len())?;
            for (at, (t, cpu)) in times.iter().zip(cpu).enumerate() {
                if step + at >= warm_up {
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

/// Milliseconds as whole nanoseconds, for the budget report.
fn nanoseconds(ms: &[f64]) -> Vec<u64> {
    // Milliseconds of GPU time, far inside a u64 of nanoseconds; never negative (`span` clamps).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    ms.iter().map(|m| (m * 1e6).round() as u64).collect()
}

/// The specs of `layers` at the probe size or the real one.
fn specs(layers: &[FaceLayerId], probe: bool) -> Vec<FaceLayerSpec> {
    let projection = mock_projection();
    let base = if probe { PROBE_WIDTH_PX / projection.rect.width() } else { DEFAULT_PX_PER_M };
    layers.iter().map(|&id| FaceLayerSpec::for_layer(id, &projection, base)).collect()
}

/// Times every case with `bench`, prints their table and returns their metrics: the steady reading's under each case's
/// key, the paced reading's under `<key>.paced`.
fn table(
    bench: &Bench<'_>,
    painter: &mut RealPainter,
    neutral: &Frame,
    args: &Arguments,
) -> Result<Vec<Metric>, String> {
    let suffix = if bench.pace.is_some() { ".paced" } else { "" };
    println!();
    println!(
        "| case | layers | sections redone | GPU raster | GPU resolve | GPU total | CPU (encode + record) | budget ms | GPU p95 within budget |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---|");
    let mut metrics = Vec::new();
    for case in &CASES {
        let changed = frame(painter, case.change)?;
        let specs = specs(case.layers, args.probe);
        let mut timed =
            bench.case(&specs, [&changed, neutral], args.frames).map_err(|e| format!("{}: {e}", case.name))?;
        metrics.extend(Metric::from_ns(&format!("{}{suffix}", case.key), &nanoseconds(&timed.total), 0));
        if case.key == BUDGET_KEY {
            metrics.extend(Metric::from_ns(&format!("cpu.face_paint{suffix}"), &nanoseconds(&timed.cpu), 0));
        }
        timed.total.sort_by(f64::total_cmp);
        let within = percentile(&timed.total, 95) <= BUDGET_MS;
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {BUDGET_MS} | {} |",
            case.name,
            case.layers.len(),
            timed.sections,
            spread(&mut timed.raster),
            spread(&mut timed.resolve),
            spread(&mut timed.total),
            spread(&mut timed.cpu),
            if within { "yes" } else { "no" }
        );
    }
    Ok(metrics)
}

fn run() -> Result<(), String> {
    let args = arguments()?;
    let budgets = parse_spec(SPEC).map_err(|e| format!("the built-in spec's §10 table: {e}"))?;
    let provenance = Provenance::from_env();
    Provenance::require(provenance.as_ref(), args.class).map_err(|e| e.to_string())?;
    let previous = match &args.previous {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Some(Report::from_json(&text).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let (device, queue, backend, software) = device()?;
    if software && args.class != Class::Reported {
        return Err(format!(
            "this is a software adapter: its numbers are never a timing (decision 7.3), so they can't be `{}`",
            format!("{:?}", args.class).to_lowercase()
        ));
    }
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let neutral = frame(&mut painter, |_| {})?;
    let stamps = Stamps::new(&device, &queue);
    let load = Load::new(&device);
    load.calibrate(&device, &queue, &stamps)?;
    let sizes: Vec<String> = specs(&FaceLayerId::ALL, args.probe)
        .iter()
        .map(|s| format!("{:?} {} x {} px", s.id, s.size_px().0, s.size_px().1))
        .collect();

    println!(
        "Graphics backend: {backend:?}{}. Layers {}; {} repaints per case after {WARM_UP} warm-up, each after a \
         {LOAD_MS} ms GPU load. Times in ms, p50 / p95.",
        if software { " (a software adapter: not a timing)" } else { "" },
        sizes.join(", "),
        args.frames
    );
    let steady = Bench { device: &device, queue: &queue, stamps: &stamps, load: &load, pace: None };
    let mut metrics = table(&steady, &mut painter, &neutral, &args)?;
    if let Some(fps) = args.paced {
        println!();
        println!(
            "Paced at {fps} fps: {} repaints per case after a {PACED_WARM_UP_S} s warm-up, each alone with no load \
             (the paced reading of spec §10, reported beside the steady one and not judged). Times in ms, p50 / p95.",
            args.frames
        );
        let paced = Bench { pace: Some(Duration::from_secs_f64(1.0 / f64::from(fps))), ..steady };
        metrics.extend(table(&paced, &mut painter, &neutral, &args)?);
    }

    let mut notes = vec![format!(
        "`{BUDGET_KEY}` is the worst case: every section of every face layer repainted with every effect showing, each \
         repaint after a {LOAD_MS} ms GPU load (the steady-clock reading of spec §10); the other `{BUDGET_KEY}.*` rows \
         are the lighter cases, reported only. Graphics backend {backend:?}.",
    )];
    if let Some(fps) = args.paced {
        notes.push(format!(
            "the `.paced` rows time the same repaints at {fps} fps with no load, the paced reading of spec §10: the GPU \
             idles between repaints, so they show its clock state on top of the face's work; reported, never judged"
        ));
    }
    if args.probe {
        notes.push(format!("probe size ({PROBE_WIDTH_PX} px wide), not the layers' real size"));
    }
    if software {
        notes.push("a software adapter: these numbers check that the probe runs and are not a timing".to_owned());
    }
    let report = Report::build(Machine::current(), metrics, &budgets, previous.as_ref(), args.class, notes)
        .with_provenance(provenance);
    if let Some(dir) = &args.report {
        let write = |name: &str, text: String| {
            let path = dir.join(name);
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
        };
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        write("report.md", report.to_markdown())?;
        write("report.json", report.to_json().map_err(|e| e.to_string())?)?;
        println!();
        println!("{}", report.to_markdown());
    }
    if args.strict {
        if matches!(args.class, Class::Measured | Class::Scaled) && report.judged() == 0 {
            return Err(format!("nothing was judged: the `{BUDGET_KEY}` row doesn't apply to the stated tier"));
        }
        let failures = report.failures();
        if !failures.is_empty() {
            return Err(format!("over budget: {}", failures.join("; ")));
        }
    }
    Ok(())
}
