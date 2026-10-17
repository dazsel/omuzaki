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

use std::process::ExitCode;

use probe::Setup;

mod percentile;
mod probe;

#[path = "../../tests/track_support/mod.rs"]
mod track_support;

fn main() -> ExitCode {
    let setup =
        Setup { models: std::env::var_os("OMUZAKI_TRACK_MODELS"), cache: std::env::var_os("OMUZAKI_ORT_CACHE") };
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
