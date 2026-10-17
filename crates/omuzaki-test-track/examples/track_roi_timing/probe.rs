//! The probe itself, in a file of its own so `tests/track_roi_probe.rs` can run it (an example's own `#[test]`s are
//! not run). Everything it prints, to the table or as an error, is free of paths: this repository is public.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use omuzaki_ai_api::{Provider, SessionOptions};
use omuzaki_ai_ort::{OrtRuntime, builds};
use omuzaki_core::time::Seconds;
use omuzaki_track::models::ModelDir;
use omuzaki_track::{FaceTracker, FaceTrackerConfig, Mode, NormRect, RgbFrame};

use crate::percentile::nearest_rank;
use crate::track_support::DirSource;

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
/// What an error shows in place of a folder the user named.
const HIDDEN: &str = "<folder>";

/// Where the models and the ONNX Runtime archive are: the two environment variables, or `None` for unset.
pub struct Setup {
    /// `OMUZAKI_TRACK_MODELS`: the folder `tools/models/mediapipe-face.sh` writes.
    pub models: Option<OsString>,
    /// `OMUZAKI_ORT_CACHE`: the folder holding the pinned ONNX Runtime archive.
    pub cache: Option<OsString>,
}

impl Setup {
    /// The places an error must not name: the two folders and the working folder.
    fn hidden(&self) -> Vec<String> {
        let cwd = std::env::current_dir().ok().map(PathBuf::into_os_string);
        [self.models.as_ref(), self.cache.as_ref(), cwd.as_ref()]
            .into_iter()
            .flatten()
            .map(|place| place.to_string_lossy().into_owned())
            .collect()
    }
}

/// `message` with every place in `hidden` replaced by [`HIDDEN`], longest first so a folder inside another is not left
/// half showing. A place with nothing in it to leak (empty, or only dots and separators, like `.` or `/`) is left
/// alone, since replacing it would only mangle the text. Errors from the model folder, the archive source and the
/// runtime name the files they could not read, so every error of the probe goes through here.
pub fn redact(message: &str, hidden: &[String]) -> String {
    let mut places: Vec<&str> =
        hidden.iter().map(String::as_str).filter(|p| p.chars().any(|c| !matches!(c, '.' | '/' | '\\'))).collect();
    places.sort_by_key(|place| std::cmp::Reverse(place.len()));
    places.iter().fold(message.to_owned(), |text, place| text.replace(place, HIDDEN))
}

fn say(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}").map_err(|e| format!("writing the table: {e}"))
}

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

/// Runs the probe and writes its table to `out`. With either folder unset it says why and succeeds, as the gated
/// tests do. Every error returned is free of paths (see [`redact`]).
pub fn run(setup: &Setup, out: &mut dyn Write) -> Result<(), String> {
    let (Some(models), Some(cache)) = (&setup.models, &setup.cache) else {
        return say(
            out,
            "skipped: set OMUZAKI_TRACK_MODELS (the model folder) and OMUZAKI_ORT_CACHE (the ONNX Runtime archive)",
        );
    };
    measure(models, cache, out).map_err(|message| redact(&message, &setup.hidden()))
}

/// Loads the graphs and prints the timing table. Its errors may name paths: [`run`] redacts them.
fn measure(models: &OsStr, cache: &OsStr, out: &mut dyn Write) -> Result<(), String> {
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
    say(out, &format!("## track_roi timing, {W}x{H}, {THREADS} thread, {CALLS} calls after {WARM_UP} warm-up\n"))?;
    say(out, "| stage | mean (ms) | p50 (ms) | p95 (ms) |\n|---|---|---|---|")?;
    say(
        out,
        &format!(
            "| total | {:.2} | {:.2} | {:.2} |\n",
            mean / 1e3,
            nearest_rank(&micros, 50) / 1e3,
            nearest_rank(&micros, 95) / 1e3
        ),
    )?;
    say(out, &format!("- ORT build: {runtime}"))?;
    say(out, "- graph optimisation: ONNX Runtime's default (`omuzaki-ai-ort` sets no level)")?;
    say(out, &format!("- platform: {}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

/// Prints the pinned ONNX Runtime archive for this platform: the URL, SHA-256 and size to fetch and check.
pub fn pins(out: &mut dyn Write) -> Result<(), String> {
    let build = builds::select_here(&[]).ok_or("no pinned ONNX Runtime build for this platform")?;
    let pin = build.pin();
    say(out, &format!("asset {}\nurl {}\nsha256 {}\nsize {}", build.asset, pin.urls.join(" "), pin.sha256, pin.size))
}
