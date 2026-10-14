//! `track_roi` on a 640x480 frame with the real graphs, one intra-op thread: the cost of landmarks, blendshapes and head
//! pose per frame in the steady state (the detector runs only when a face is new). Criterion reports the mean; the p95
//! is taken over a separate pass of timed calls and printed beside it. Spec v3 §10 budgets `track.face` at 5 ms p95.
//!
//! Gated on `OMUZAKI_TRACK_MODELS` and `OMUZAKI_ORT_CACHE`, as `tests/track_models.rs`; prints a line and exits when one
//! is unset.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a bench that cannot run should stop

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use criterion::{Criterion, criterion_group};
use omuzaki_ai_api::{FetchError, FileSource, PinnedFile, Progress, SessionOptions};
use omuzaki_ai_ort::OrtRuntime;
use omuzaki_core::time::Seconds;
use omuzaki_track::models::ModelDir;
use omuzaki_track::{FaceTracker, FaceTrackerConfig, Mode, NormRect, RgbFrame};

/// Archives from the `OMUZAKI_ORT_CACHE` folder under their release file names; never downloads.
struct DirSource(PathBuf);

impl FileSource for DirSource {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        let asset = pin.urls[0].rsplit('/').next().unwrap();
        let path = self.0.join(asset);
        let io = |e: std::io::Error| FetchError::Io { path: path.display().to_string(), reason: e.to_string() };
        let actual = std::fs::metadata(&path).map_err(io)?.len();
        if actual != pin.size {
            return Err(FetchError::Size { url: asset.to_owned(), expected: pin.size, actual });
        }
        Ok(path)
    }
}

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
    let options = SessionOptions { threads: 1, ..SessionOptions::default() };
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
    let mut run = |tracker: &mut FaceTracker| {
        index += 1;
        let result = tracker.track_roi(&frame, ROI, Seconds(f64::from(index) / 30.0));
        black_box(result.is_ok())
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
        "track_roi 640x480, 1 thread, 1000 calls: mean {:.2} ms, p50 {:.2} ms, p95 {:.2} ms (spec §10: 5 ms p95)",
        mean / 1e3,
        micros[micros.len() / 2] / 1e3,
        micros[micros.len() * 95 / 100] / 1e3
    );

    c.bench_function("track_roi_640x480_1_thread", |b| b.iter(|| run(&mut tracker)));
}

criterion_group!(benches, bench_track_roi);
criterion::criterion_main!(benches);
