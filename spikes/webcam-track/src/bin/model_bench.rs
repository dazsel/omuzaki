//! Raw inference latency of the three ONNX models under `ort`, with no pre- or post-processing: the numbers to set
//! against TFLite/XNNPACK (`tools/bench_tflite.py`) and against the full pipeline (`track`).
//!
//!     model_bench --ort-lib libonnxruntime.so --models DIR [--backend cuda] [--threads 4] [--iters 300]

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, ValueEnum};
use webcam_track::error::Result;
use webcam_track::runtime::{Backend, Model, RuntimeOptions, init_runtime};
use webcam_track::timing::Summary;

#[derive(Clone, Copy, ValueEnum)]
enum BackendArg {
    Cpu,
    Cuda,
}

/// Times `Model::run` on a fixed input for each of the three face models.
#[derive(Parser)]
struct Args {
    /// Path to libonnxruntime.so.
    #[arg(long)]
    ort_lib: PathBuf,
    /// Directory with the ONNX models.
    #[arg(long)]
    models: PathBuf,
    /// Execution provider.
    #[arg(long, value_enum, default_value = "cpu")]
    backend: BackendArg,
    /// CPU threads per operator.
    #[arg(long, default_value_t = 1)]
    threads: usize,
    /// Make worker threads sleep between operators instead of busy-waiting.
    #[arg(long)]
    no_spin: bool,
    /// Timed iterations per model (after 20 untimed).
    #[arg(long, default_value_t = 300)]
    iters: usize,
}

/// (file, input shape) of each model.
const MODELS: [(&str, &[usize]); 3] = [
    ("face_detector.onnx", &[1, 128, 128, 3]),
    ("face_landmarks_detector.onnx", &[1, 256, 256, 3]),
    ("face_blendshapes.onnx", &[1, 146, 2]),
];

fn run(args: &Args) -> Result<()> {
    init_runtime(&args.ort_lib)?;
    let backend = match args.backend {
        BackendArg::Cpu => Backend::Cpu,
        BackendArg::Cuda => Backend::Cuda { device: 0 },
    };
    let options = RuntimeOptions { backend, intra_threads: args.threads, spin: !args.no_spin };
    for (file, shape) in MODELS {
        let mut model = Model::load(&args.models.join(file), &options)?;
        let input: Vec<f32> = (0..shape.iter().product::<usize>())
            .map(|i| f32::from(u8::try_from((i * 7919) % 256).unwrap_or(0)) / 255.0)
            .collect();
        for _ in 0..20 {
            model.run(shape, &input)?;
        }
        let mut seconds = Vec::with_capacity(args.iters);
        for _ in 0..args.iters {
            let t = Instant::now();
            model.run(shape, &input)?;
            seconds.push(t.elapsed().as_secs_f64());
        }
        if let Some(s) = Summary::of_seconds(&seconds) {
            println!("{file:<32} mean {:>7.3} ms  p50 {:>7.3}  p95 {:>7.3}  max {:>7.3}", s.mean, s.p50, s.p95, s.max);
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run(&Args::parse()) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
