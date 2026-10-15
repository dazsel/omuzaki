//! `track_roi` on a 640x480 frame with the real graphs, one intra-op thread: the cost of landmarks, blendshapes and head
//! pose per frame in the steady state (the detector runs only when a face is new). Criterion reports the mean; the p95
//! is taken over a separate pass of timed calls and printed beside it. This is a `track_roi` microbenchmark only: it leaves out
//! the detector, the frame hand-off and the rest of the `track.face` path, so it is not compared with that budget.
//!
//! Gated on `OMUZAKI_TRACK_MODELS` and `OMUZAKI_ORT_CACHE`, as `tests/track_models.rs`; prints a line and exits when one
//! is unset.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a bench that cannot run should stop

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use criterion::{Criterion, criterion_group};
use omuzaki_ai_api::{Provider, SessionOptions};
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
    let (Some(models), Some(cache)) = (std::env::var_os("OMUZAKI_TRACK_MODELS"), std::env::var_os("OMUZAKI_ORT_CACHE"))
    else {
        eprintln!("skipping the track bench: set OMUZAKI_TRACK_MODELS (the model folder) and OMUZAKI_ORT_CACHE");
        return;
    };
    let dir = ModelDir::open(&PathBuf::from(models)).expect("the model folder opens");
    let options = SessionOptions::new(vec![Provider::Cpu], 1);
    let graphs = dir.load(&OrtRuntime, &DirSource(PathBuf::from(cache)), &options).expect("the graphs load");
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
        "track_roi 640x480, 1 thread, 1000 calls: mean {:.2} ms, p50 {:.2} ms, p95 {:.2} ms",
        mean / 1e3,
        micros[micros.len() / 2] / 1e3,
        micros[micros.len() * 95 / 100] / 1e3
    );

    c.bench_function("track_roi_640x480_1_thread", |b| b.iter(|| run(&mut tracker)));
}

criterion_group!(benches, bench_track_roi);
criterion::criterion_main!(benches);
