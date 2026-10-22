//! The `track_roi` timing probe for the reference machine (#1515 part B, #903): `track_roi` on a synthetic 640x480
//! frame with the real graphs, 20 warm-up calls and then 1,000 timed ones per runtime and thread count (1, 2 and 4).
//! It prints Markdown tables of the whole call (mean, p50, p95, and the process's CPU time per call) and of each
//! stage (warp, landmark run, decode, blendshapes, smoothing and pose; #1905), and the builds that ran them:
//! - ONNX Runtime, over the converted model folder;
//! - LiteRT with XNNPACK, over MediaPipe's official `face_landmarker.task`.
//!
//! ```text
//! track_roi_timing            # ONNX Runtime: OMUZAKI_TRACK_MODELS (the folder tools/models/mediapipe-face.sh writes)
//!                             #   and OMUZAKI_ORT_CACHE (a folder holding the pinned ONNX Runtime archive)
//!                             # LiteRT: OMUZAKI_FACE_TASK (the downloaded face_landmarker.task),
//!                             #   OMUZAKI_LITERT_CACHE (a folder holding the CI-built LiteRT archive)
//!                             #   and OMUZAKI_LITERT_SHA256 (that CI run's SHA-256 of it)
//! track_roi_timing --pins     # prints the pinned ONNX Runtime archive and the face bundle, to fetch and check by hand
//! ```
//!
//! With neither set of variables it prints why and exits 0, as the gated tests do. It never downloads: the archives
//! and the models come from pinned, checksummed sources and are checked again on load.
//! The output holds no machine name, CPU name or path, because it is posted in a public repository.
#![allow(clippy::doc_markdown)] // LiteRT, XNNPACK and MediaPipe are product names

use std::process::ExitCode;

use probe::Setup;

mod cpu;
mod percentile;
mod probe;

#[path = "../../tests/track_support/mod.rs"]
mod track_support;

fn main() -> ExitCode {
    let setup = Setup {
        models: std::env::var_os("OMUZAKI_TRACK_MODELS"),
        cache: std::env::var_os("OMUZAKI_ORT_CACHE"),
        task: std::env::var_os("OMUZAKI_FACE_TASK"),
        litert: std::env::var_os("OMUZAKI_LITERT_CACHE"),
        litert_sha256: std::env::var("OMUZAKI_LITERT_SHA256").ok(),
    };
    let mut out = std::io::stdout().lock();
    let result = if std::env::args().nth(1).as_deref() == Some("--pins") {
        probe::pins(&mut out)
    } else {
        probe::run(&setup, &mut out)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("track_roi_timing: {message}");
            ExitCode::FAILURE
        }
    }
}
