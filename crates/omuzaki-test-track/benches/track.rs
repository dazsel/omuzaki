//! `track_roi` on a 640x480 frame with the real graphs: the cost of landmarks, blendshapes and head
//! pose per frame in the steady state (the detector runs only when a face is new). Criterion reports the mean; the p95
//! is taken over a separate pass of timed calls and printed beside it. This is a `track_roi` microbenchmark only: it leaves out
//! the detector, the frame hand-off and the rest of the `track.face` path, so it is not compared with that budget.
//!
//! Two runtimes, each gated on its own inputs; a runtime whose inputs are unset prints a line and is skipped:
//! - `onnx`: `OMUZAKI_TRACK_MODELS` (the converted folder) and `OMUZAKI_ORT_CACHE`, as `tests/track_models.rs`;
//! - `tflite` (LiteRT with XNNPACK, #903): `OMUZAKI_FACE_TASK` (the official `face_landmarker.task`, unpacked beside a
//!   copy in a scratch folder), `OMUZAKI_LITERT_CACHE` and `OMUZAKI_LITERT_SHA256` (the archive of the CI run that
//!   built it, and that run's digest; there is no published pin yet, #1748). LiteRT runs at 1 and 2 threads.
#![allow(clippy::doc_markdown)] // LiteRT and XNNPACK are product names
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a bench that cannot run should stop

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use criterion::{Criterion, criterion_group};
use omuzaki_ai_api::{FileSource, Provider, Runtime, SessionOptions};
use omuzaki_ai_litert::LiteRtRuntime;
use omuzaki_ai_ort::OrtRuntime;
use omuzaki_core::time::Seconds;
use omuzaki_track::models::ModelDir;
use omuzaki_track::{FaceTracker, FaceTrackerConfig, Mode, NormRect, RgbFrame};
use track_support::DirSource;

#[path = "../tests/track_support/mod.rs"]
mod track_support;

const W: u32 = 640;
const H: u32 = 480;
const ROI: NormRect = NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.667, rotation: 0.0 };

fn bench_track_roi(c: &mut Criterion) {
    if let (Some(models), Some(cache)) =
        (std::env::var_os("OMUZAKI_TRACK_MODELS"), std::env::var_os("OMUZAKI_ORT_CACHE"))
    {
        let dir = ModelDir::open(&PathBuf::from(models)).expect("the model folder opens");
        bench_on(c, "onnx", &dir, &OrtRuntime, &DirSource(PathBuf::from(cache)), 1);
    } else {
        eprintln!("skipping the onnx track bench: set OMUZAKI_TRACK_MODELS (the model folder) and OMUZAKI_ORT_CACHE");
    }

    let (Some(task), Some(cache)) = (std::env::var_os("OMUZAKI_FACE_TASK"), std::env::var_os("OMUZAKI_LITERT_CACHE"))
    else {
        eprintln!(
            "skipping the tflite track bench: set OMUZAKI_FACE_TASK, OMUZAKI_LITERT_CACHE and OMUZAKI_LITERT_SHA256"
        );
        return;
    };
    let build = omuzaki_ai_litert::builds::select_here().expect("a LiteRT build for this platform");
    let archive = PathBuf::from(cache).join(build.asset);
    let sha256 = std::env::var("OMUZAKI_LITERT_SHA256").expect("OMUZAKI_LITERT_SHA256: the build run's digest");
    let actual = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(std::fs::read(&archive).expect("the archive")));
    assert_eq!(actual, sha256.to_lowercase(), "refusing the LiteRT archive: its SHA-256 differs");
    omuzaki_ai_litert::load_archive(&archive, &sha256).expect("LiteRT loads");
    let scratch = tempfile::tempdir().expect("a scratch folder");
    let copy = scratch.path().join(omuzaki_track::task::TASK_FILE);
    std::fs::copy(task, &copy).expect("the bundle copies");
    let dir = omuzaki_track::task::unpack(&copy).expect("the official bundle unpacks");
    for threads in [1, 2] {
        bench_on(c, "tflite", &dir, &LiteRtRuntime, &DirSource(scratch.path().to_path_buf()), threads);
    }
}

/// `track_roi` on `dir`'s graphs through `runtime` at `threads` intra-op threads.
fn bench_on(
    c: &mut Criterion,
    name: &str,
    dir: &ModelDir,
    runtime: &dyn Runtime,
    natives: &dyn FileSource,
    threads: u16,
) {
    let options = SessionOptions::new(vec![Provider::Cpu], threads);
    let graphs = dir.load(runtime, natives, &options).expect("the graphs load");
    let config = FaceTrackerConfig { mode: Mode::Video, ..FaceTrackerConfig::default() };
    let mut tracker = FaceTracker::new(graphs, &dir.geometry().unwrap(), config).expect("the graphs fit their slots");

    let mut pixels = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            pixels.extend([(x * 255 / (W - 1)) as u8, (y * 255 / (H - 1)) as u8, ((x + y) % 256) as u8]);
        }
    }
    let frame = RgbFrame { data: &pixels, width: W, height: H, stride: (W * 3) as usize };
    let mut index = 0_u32;
    // Every call must succeed: a failed call would time partial work, so it stops the bench.
    let mut run = |tracker: &mut FaceTracker| {
        index += 1;
        let sample = tracker.track_roi(&frame, ROI, Seconds(f64::from(index) / 30.0));
        black_box(sample.unwrap_or_else(|e| panic!("track_roi call {index} failed: {e}")))
    };
    for _ in 0..20 {
        run(&mut tracker); // warm-up: buffers grown, lazies initialised
    }

    let mut micros: Vec<f64> = (0..1000)
        .map(|_| {
            let started = Instant::now();
            run(&mut tracker);
            started.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    micros.sort_by(f64::total_cmp);
    let mean = micros.iter().sum::<f64>() / micros.len() as f64;
    eprintln!(
        "track_roi {name} 640x480, {threads} thread(s), 1000 calls: mean {:.2} ms, p50 {:.2} ms, p95 {:.2} ms",
        mean / 1e3,
        micros[micros.len() / 2] / 1e3,
        micros[micros.len() * 95 / 100] / 1e3
    );

    c.bench_function(&format!("track_roi_{name}_640x480_{threads}_thread"), |b| b.iter(|| run(&mut tracker)));
}

criterion_group!(benches, bench_track_roi);
criterion::criterion_main!(benches);
