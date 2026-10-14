//! Runs the tracker over a directory of PNG frames and writes per-frame blendshapes, head pose and (optionally)
//! landmarks as JSON in the shape of the earlier web prototype's `track_face.py`, plus latency and CPU numbers.
//!
//!     track --ort-lib libonnxruntime.so --models DIR --input frames/ --out result.json [--backend cuda] [--mode video]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use serde::Serialize;
use webcam_track::blendshapes::NAMES;
use webcam_track::error::{Result, TrackError};
use webcam_track::frame::Frame;
use webcam_track::pipeline::{FaceTracker, Mode, StageTimings, TrackerConfig};
use webcam_track::runtime::{Backend, RuntimeOptions, init_runtime};
use webcam_track::timing::{CpuMeter, Summary};

#[derive(Clone, Copy, ValueEnum)]
enum BackendArg {
    Cpu,
    Cuda,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Image,
    Video,
}

/// Face tracking benchmark and recorder for the spike.
#[derive(Parser)]
struct Args {
    /// Path to libonnxruntime.so (Microsoft's release build; see models/PROVENANCE.md).
    #[arg(long)]
    ort_lib: PathBuf,
    /// Directory with the ONNX models and geometry metadata.
    #[arg(long)]
    models: PathBuf,
    /// Directory of PNG frames, processed in file-name order.
    #[arg(long)]
    input: PathBuf,
    /// Where to write the JSON result.
    #[arg(long)]
    out: PathBuf,
    /// Execution provider.
    #[arg(long, value_enum, default_value = "cpu")]
    backend: BackendArg,
    /// CPU threads per operator.
    #[arg(long, default_value_t = 1)]
    threads: usize,
    /// Make worker threads sleep between operators instead of busy-waiting (slower here, see the Findings in README.md).
    #[arg(long)]
    no_spin: bool,
    /// `image`: independent frames; `video`: track between frames and smooth.
    #[arg(long, value_enum, default_value = "image")]
    mode: ModeArg,
    /// Detection and presence confidence thresholds.
    #[arg(long, default_value_t = 0.5)]
    conf: f32,
    /// Frame rate of the input, for timestamps and pacing.
    #[arg(long, default_value_t = 30.0)]
    fps: f64,
    /// Untimed passes over the first frames before measuring (session warm-up, allocator, caches).
    #[arg(long, default_value_t = 10)]
    warmup: usize,
    /// Extra timed passes over the whole input (the JSON holds the first pass only).
    #[arg(long, default_value_t = 0)]
    repeat: usize,
    /// Sleep between frames to hold `--fps`, so the CPU figure is what a live camera would cost.
    #[arg(long)]
    pace: bool,
    /// Include the 478 landmarks in the JSON.
    #[arg(long)]
    landmarks: bool,
}

#[derive(Serialize)]
struct FrameOut {
    t: f64,
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    bs: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    m: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lm: Option<Vec<[f32; 3]>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence: Option<f32>,
    /// The landmark ROI as [x_center, y_center, width, height, rotation], normalised; for debugging.
    #[serde(skip_serializing_if = "Option::is_none")]
    roi: Option<[f32; 5]>,
    detector_ran: bool,
    total_ms: f64,
}

#[derive(Serialize)]
struct Stages {
    total: Option<Summary>,
    detector_preprocess: Option<Summary>,
    detector_inference: Option<Summary>,
    detector_decode: Option<Summary>,
    landmark_preprocess: Option<Summary>,
    landmark_inference: Option<Summary>,
    landmark_decode: Option<Summary>,
    smoothing: Option<Summary>,
    blendshape_inference: Option<Summary>,
    pose: Option<Summary>,
}

#[derive(Serialize)]
struct Latency {
    /// Frames where the detector ran and found a face.
    with_detector: Stages,
    /// Frames tracked from the previous frame (video mode only).
    tracked: Stages,
    cpu_percent_of_one_core: Option<f64>,
    wall_seconds: Option<f64>,
    timed_frames: usize,
}

#[derive(Serialize)]
struct Output {
    fps: f64,
    source: String,
    start: f64,
    names: Vec<&'static str>,
    frames: Vec<FrameOut>,
    detect_rate: f64,
    settings: serde_json::Value,
    latency: Latency,
}

fn load_frames(dir: &Path) -> Result<Vec<Frame>> {
    let io = |source| TrackError::Io { path: dir.to_path_buf(), source };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(io)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
        .collect();
    files.sort();
    files.iter().map(|f| Frame::from_png(f)).collect()
}

fn stages(timings: &[&StageTimings]) -> Stages {
    let pick =
        |f: &dyn Fn(&StageTimings) -> f64| Summary::of_seconds(&timings.iter().map(|t| f(t)).collect::<Vec<_>>());
    Stages {
        total: pick(&|t| t.total),
        detector_preprocess: pick(&|t| t.detector.preprocess),
        detector_inference: pick(&|t| t.detector.inference),
        detector_decode: pick(&|t| t.detector.decode),
        landmark_preprocess: pick(&|t| t.landmarks.preprocess),
        landmark_inference: pick(&|t| t.landmarks.inference),
        landmark_decode: pick(&|t| t.landmarks.decode),
        smoothing: pick(&|t| t.smoothing),
        blendshape_inference: pick(&|t| t.blendshapes),
        pose: pick(&|t| t.pose),
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // frame indices are tiny
fn run(args: &Args) -> Result<()> {
    init_runtime(&args.ort_lib)?;
    let backend = match args.backend {
        BackendArg::Cpu => Backend::Cpu,
        BackendArg::Cuda => Backend::Cuda { device: 0 },
    };
    let mode = match args.mode {
        ModeArg::Image => Mode::Image,
        ModeArg::Video => Mode::Video,
    };
    let config = TrackerConfig {
        models: args.models.clone(),
        runtime: RuntimeOptions { backend, intra_threads: args.threads, spin: !args.no_spin },
        mode,
        min_detection_confidence: args.conf,
        min_presence_confidence: args.conf,
    };
    let frames = load_frames(&args.input)?;
    if frames.is_empty() {
        return Err(TrackError::Input(format!("no PNG frames in {}", args.input.display())));
    }
    let mut tracker = FaceTracker::new(&config)?;

    // Warm-up: untimed, on throwaway state, so first-call costs (kernel selection, memory arenas) stay out of the numbers.
    for i in 0..args.warmup {
        tracker.track(&frames[i % frames.len()], (i as f64 * 1000.0 / args.fps) as i64)?;
    }
    tracker.reset();

    let frame_period = Duration::from_secs_f64(1.0 / args.fps);
    let meter = CpuMeter::start();
    let started = Instant::now();
    let mut out_frames = Vec::with_capacity(frames.len());
    let mut all_timings: Vec<(bool, StageTimings)> = Vec::new(); // (found a face, timings)
    for pass in 0..=args.repeat {
        for (i, frame) in frames.iter().enumerate() {
            let index = pass * frames.len() + i;
            let t = index as f64 / args.fps;
            let (result, timings) = tracker.track(frame, (t * 1000.0) as i64)?;
            all_timings.push((result.is_some(), timings));
            if pass == 0 {
                out_frames.push(FrameOut {
                    t: (t * 1e4).round() / 1e4,
                    found: result.is_some(),
                    bs: result.as_ref().map(|r| r.blendshapes.to_vec()),
                    m: result.as_ref().map(|r| r.pose.to_vec()),
                    lm: result.as_ref().filter(|_| args.landmarks).map(|r| r.landmarks.clone()),
                    presence: result.as_ref().map(|r| r.presence),
                    roi: result
                        .as_ref()
                        .map(|r| [r.roi.x_center, r.roi.y_center, r.roi.width, r.roi.height, r.roi.rotation]),
                    detector_ran: timings.detector_ran,
                    total_ms: timings.total * 1000.0,
                });
            }
            if let Some(wait) = args
                .pace
                .then(|| (started + frame_period * (index as u32 + 1)).checked_duration_since(Instant::now()))
                .flatten()
            {
                std::thread::sleep(wait);
            }
        }
    }
    let (cpu, wall) = meter.finish().unzip();

    let found = |detector: bool| -> Vec<&StageTimings> {
        all_timings.iter().filter(|(ok, t)| *ok && t.detector_ran == detector).map(|(_, t)| t).collect()
    };
    let detect_rate = out_frames.iter().filter(|f| f.found).count() as f64 / out_frames.len() as f64;
    let output = Output {
        fps: args.fps,
        source: args.input.display().to_string(),
        start: 0.0,
        names: NAMES[1..].to_vec(),
        frames: out_frames,
        detect_rate,
        settings: serde_json::json!({
            "backend": format!("{backend:?}"), "threads": args.threads, "spin": !args.no_spin,
            "mode": format!("{mode:?}"), "confidence": args.conf, "pace": args.pace, "warmup": args.warmup, "repeat": args.repeat,
            "ort_lib": args.ort_lib.display().to_string(),
        }),
        latency: Latency {
            with_detector: stages(&found(true)),
            tracked: stages(&found(false)),
            cpu_percent_of_one_core: cpu,
            wall_seconds: wall,
            timed_frames: all_timings.len(),
        },
    };
    let json = serde_json::to_string(&output).map_err(|e| TrackError::Input(e.to_string()))?;
    std::fs::write(&args.out, json).map_err(|source| TrackError::Io { path: args.out.clone(), source })?;
    print_summary(&output);
    Ok(())
}

fn print_summary(o: &Output) {
    println!(
        "frames: {}  detect rate: {:.1}%  ({} timed calls)",
        o.frames.len(),
        o.detect_rate * 100.0,
        o.latency.timed_frames
    );
    for (label, s) in [("detector ran", &o.latency.with_detector), ("tracked", &o.latency.tracked)] {
        let Some(total) = s.total else { continue };
        println!(
            "[{label}] n={}  total mean {:.2} ms  p50 {:.2}  p95 {:.2}  max {:.2}",
            total.n, total.mean, total.p50, total.p95, total.max
        );
        for (name, v) in [
            ("detector preprocess", s.detector_preprocess),
            ("detector inference", s.detector_inference),
            ("detector decode", s.detector_decode),
            ("landmark preprocess", s.landmark_preprocess),
            ("landmark inference", s.landmark_inference),
            ("landmark decode", s.landmark_decode),
            ("smoothing", s.smoothing),
            ("blendshape inference", s.blendshape_inference),
            ("pose fit", s.pose),
        ] {
            if let Some(v) = v {
                println!("    {name:<22} mean {:>7.3} ms  p95 {:>7.3}", v.mean, v.p95);
            }
        }
    }
    if let (Some(cpu), Some(wall)) = (o.latency.cpu_percent_of_one_core, o.latency.wall_seconds) {
        println!("CPU: {cpu:.1}% of one core over {wall:.1} s wall");
    }
}

fn main() {
    let args = Args::parse();
    if let Err(e) = run(&args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
