//! The `track_roi` timing probe for the reference machine (#1515 part B): `track_roi` on a synthetic 640x480 frame with
//! the real graphs and one intra-op thread, 20 warm-up calls and then 1,000 timed ones. It prints a Markdown table of
//! the mean, p50 and p95 of the whole call, and the ONNX Runtime build that ran it. Per-stage times (warp, landmark
//! run, decode) need a hook in `omuzaki-track` and come later.
//!
//! ```text
//! track_roi_timing            # needs OMUZAKI_TRACK_MODELS (the folder tools/models/mediapipe-face.sh writes)
//!                             # and OMUZAKI_ORT_CACHE (a folder holding the pinned ONNX Runtime archive)
//! track_roi_timing --pins     # prints the pinned ONNX Runtime archive for this platform, to fetch and check by hand
//! ```
//!
//! With either variable unset it prints why and exits 0, as the gated tests do. It never downloads: the archive and
//! the models come from the pinned, checksummed sources, and `DirSource` checks the archive's size and SHA-256 again.
//! The output holds no machine name, CPU name or path, because it is posted in a public repository.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use omuzaki_ai_api::{Provider, SessionOptions};
use omuzaki_ai_ort::{OrtRuntime, builds};
use omuzaki_core::time::Seconds;
use omuzaki_track::models::ModelDir;
use omuzaki_track::{FaceTracker, FaceTrackerConfig, Mode, NormRect, RgbFrame};
use percentile::nearest_rank;
use track_support::DirSource;

mod percentile;

#[path = "../../tests/track_support/mod.rs"]
mod track_support;

const W: u32 = 640;
const H: u32 = 480;
/// A region in the middle of the frame, where a face would be.
const ROI: NormRect = NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.667, rotation: 0.0 };
/// Calls before timing starts: buffers grown, lazy state initialised.
const WARM_UP: u32 = 20;
/// Timed calls.
const CALLS: u32 = 1000;
/// Intra-op threads of every session.
const THREADS: u16 = 1;

/// A procedural frame: smooth gradients with a diagonal texture, so every pixel differs and no face is in it.
fn gradient() -> Vec<u8> {
    let mut pixels = Vec::with_capacity(W as usize * H as usize * 3);
    for y in 0..H {
        for x in 0..W {
            // Each value is at most 255 by construction, so the fallback is never taken.
            let channel = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
            pixels.extend([channel(x * 255 / (W - 1)), channel(y * 255 / (H - 1)), channel((x + y) % 256)]);
        }
    }
    pixels
}

fn run() -> Result<(), String> {
    let (Some(models), Some(cache)) = (std::env::var_os("OMUZAKI_TRACK_MODELS"), std::env::var_os("OMUZAKI_ORT_CACHE"))
    else {
        println!(
            "skipped: set OMUZAKI_TRACK_MODELS (the model folder) and OMUZAKI_ORT_CACHE (the ONNX Runtime archive)"
        );
        return Ok(());
    };
    let dir = ModelDir::open(&PathBuf::from(models)).map_err(|e| format!("the model folder: {e}"))?;
    let options = SessionOptions::new(vec![Provider::Cpu], THREADS);
    let graphs = dir
        .load(&OrtRuntime, &DirSource(PathBuf::from(cache)), &options)
        .map_err(|e| format!("loading the graphs: {e}"))?;
    let geometry = dir.geometry().map_err(|e| format!("the model geometry: {e}"))?;
    let config = FaceTrackerConfig { mode: Mode::Video, ..FaceTrackerConfig::default() };
    let mut tracker = FaceTracker::new(graphs, &geometry, config).map_err(|e| format!("the graphs' slots: {e}"))?;

    let pixels = gradient();
    let frame = RgbFrame { data: &pixels, width: W, height: H, stride: W as usize * 3 };
    let mut index = 0_u32;
    // A failed call would time partial work, so the first one stops the probe.
    let mut call = |tracker: &mut FaceTracker| -> Result<f64, String> {
        index += 1;
        let started = Instant::now();
        let sample = tracker.track_roi(&frame, ROI, Seconds(f64::from(index) / 30.0));
        let micros = started.elapsed().as_secs_f64() * 1e6;
        sample.map(std::hint::black_box).map_err(|e| format!("track_roi call {index} failed: {e}"))?;
        Ok(micros)
    };
    for _ in 0..WARM_UP {
        call(&mut tracker)?;
    }
    let mut micros = (0..CALLS).map(|_| call(&mut tracker)).collect::<Result<Vec<_>, _>>()?;
    micros.sort_by(f64::total_cmp);
    let mean = micros.iter().sum::<f64>() / f64::from(CALLS);

    let runtime = omuzaki_ai_ort::loaded().map_or_else(
        || "not reported".to_owned(),
        |l| {
            format!(
                "{} (ONNX Runtime {}{})",
                l.build.id,
                builds::ORT_VERSION,
                if l.ours { "" } else { ", shared copy" }
            )
        },
    );
    println!("## track_roi timing, {W}x{H}, {THREADS} thread, {CALLS} calls after {WARM_UP} warm-up\n");
    println!("| stage | mean (ms) | p50 (ms) | p95 (ms) |\n|---|---|---|---|");
    println!(
        "| total | {:.2} | {:.2} | {:.2} |\n",
        mean / 1e3,
        nearest_rank(&micros, 50) / 1e3,
        nearest_rank(&micros, 95) / 1e3
    );
    println!("- ORT build: {runtime}");
    println!("- graph optimisation: ONNX Runtime's default (`omuzaki-ai-ort` sets no level)");
    println!("- platform: {}-{}", std::env::consts::OS, std::env::consts::ARCH);
    Ok(())
}

/// Prints the pinned ONNX Runtime archive for this platform: the URL, SHA-256 and size to fetch and check.
fn pins() -> Result<(), String> {
    let build = builds::select_here(&[]).ok_or("no pinned ONNX Runtime build for this platform")?;
    let pin = build.pin();
    println!("asset {}\nurl {}\nsha256 {}\nsize {}", build.asset, pin.urls.join(" "), pin.sha256, pin.size);
    Ok(())
}

fn main() -> ExitCode {
    let result = if std::env::args().nth(1).as_deref() == Some("--pins") { pins() } else { run() };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("track_roi_timing: {message}");
            ExitCode::FAILURE
        }
    }
}
