//! The probe itself, in a file of its own so `tests/track_roi_probe.rs` can run it (an example's own `#[test]`s are
//! not run). Everything it prints, to the table or as an error, is free of paths: this repository is public.
#![allow(clippy::doc_markdown)] // LiteRT, XNNPACK and MediaPipe are product names

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use omuzaki_ai_api::{FileSource, Provider, Runtime, SessionOptions};
use omuzaki_ai_litert::LiteRtRuntime;
use omuzaki_ai_ort::{OrtRuntime, builds};
use omuzaki_core::time::Seconds;
use omuzaki_track::models::ModelDir;
use omuzaki_track::{FaceTracker, FaceTrackerConfig, Mode, NormRect, RgbFrame};
use sha2::Digest;

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
/// Intra-op threads of the ONNX Runtime sessions.
const THREADS: u16 = 1;
/// Intra-op threads LiteRT runs at, one row each.
const LITERT_THREADS: [u16; 2] = [1, 2];
/// What an error shows in place of a folder the user named.
const HIDDEN: &str = "<folder>";

/// Where the models and the runtimes are: the environment variables, or `None` for unset. Each runtime runs when its
/// own variables are set.
#[derive(Default)]
pub struct Setup {
    /// `OMUZAKI_TRACK_MODELS`: the folder `tools/models/mediapipe-face.sh` writes.
    pub models: Option<OsString>,
    /// `OMUZAKI_ORT_CACHE`: the folder holding the pinned ONNX Runtime archive.
    pub cache: Option<OsString>,
    /// `OMUZAKI_FACE_TASK`: MediaPipe's official `face_landmarker.task`, downloaded (`--pins` prints its URL).
    pub task: Option<OsString>,
    /// `OMUZAKI_LITERT_CACHE`: the folder holding the LiteRT archive of the CI run that built it.
    pub litert: Option<OsString>,
    /// `OMUZAKI_LITERT_SHA256`: that run's SHA-256 of the archive (there is no published pin yet, #1748).
    pub litert_sha256: Option<String>,
}

impl Setup {
    /// The places an error must not name: the folders, the bundle and the working folder.
    fn hidden(&self) -> Vec<String> {
        let cwd = std::env::current_dir().ok().map(PathBuf::into_os_string);
        [self.models.as_ref(), self.cache.as_ref(), self.task.as_ref(), self.litert.as_ref(), cwd.as_ref()]
            .into_iter()
            .flatten()
            .map(|place| place.to_string_lossy().into_owned())
            .collect()
    }
}

/// One row of the table: the runtime and its thread count, and the call's times in microseconds, sorted.
struct Row {
    runtime: &'static str,
    threads: u16,
    micros: Vec<f64>,
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

/// Runs the probe and writes its table to `out`: ONNX Runtime when its two variables are set, LiteRT when its three
/// are. With neither set it says why and succeeds, as the gated tests do. Nothing is written unless every run
/// succeeds, and every error returned is free of paths (see [`redact`]).
pub fn run(setup: &Setup, out: &mut dyn Write) -> Result<(), String> {
    let ort = setup.models.as_ref().zip(setup.cache.as_ref());
    let litert = setup.task.as_ref().zip(setup.litert.as_ref()).zip(setup.litert_sha256.as_ref());
    if ort.is_none() && litert.is_none() {
        return say(
            out,
            "skipped: set OMUZAKI_TRACK_MODELS (the model folder) and OMUZAKI_ORT_CACHE (the ONNX Runtime archive), \
             or OMUZAKI_FACE_TASK, OMUZAKI_LITERT_CACHE and OMUZAKI_LITERT_SHA256 (LiteRT)",
        );
    }
    let mut rows = Vec::new();
    let mut measured = || -> Result<(), String> {
        if let Some((models, cache)) = ort {
            rows.push(measure_ort(models, cache)?);
        }
        if let Some(((task, cache), sha256)) = litert {
            rows.extend(measure_litert(task, cache, sha256)?);
        }
        Ok(())
    };
    measured().map_err(|message| redact(&message, &setup.hidden()))?;
    table(&rows, out)
}

/// The ONNX Runtime row, over the converted model folder. Its errors may name paths: [`run`] redacts them.
fn measure_ort(models: &OsStr, cache: &OsStr) -> Result<Row, String> {
    let dir = ModelDir::open(&PathBuf::from(models)).map_err(|e| format!("the model folder: {e}"))?;
    let micros = time(&dir, &OrtRuntime, &DirSource(PathBuf::from(cache)), THREADS)?;
    Ok(Row { runtime: "onnx", threads: THREADS, micros })
}

/// The LiteRT rows, over the official bundle unpacked in a scratch folder. Its errors may name paths.
fn measure_litert(task: &OsStr, cache: &OsStr, sha256: &str) -> Result<Vec<Row>, String> {
    let build = omuzaki_ai_litert::builds::select_here().ok_or("no LiteRT build for this platform")?;
    let archive = PathBuf::from(cache).join(build.asset);
    let bytes = std::fs::read(&archive).map_err(|e| format!("the LiteRT archive {}: {e}", archive.display()))?;
    if format!("{:x}", sha2::Sha256::digest(&bytes)) != sha256.trim().to_lowercase() {
        return Err("the LiteRT archive does not match OMUZAKI_LITERT_SHA256".to_owned());
    }
    omuzaki_ai_litert::load_archive(&archive, sha256.trim()).map_err(|e| format!("loading LiteRT: {e}"))?;
    let scratch = tempfile::tempdir().map_err(|e| format!("a scratch folder: {e}"))?;
    let copy = scratch.path().join(omuzaki_track::task::TASK_FILE);
    std::fs::copy(task, &copy).map_err(|e| format!("the face bundle: {e}"))?;
    let dir = omuzaki_track::task::unpack(&copy).map_err(|e| format!("the face bundle: {e}"))?;
    let natives = DirSource(scratch.path().to_path_buf());
    LITERT_THREADS
        .into_iter()
        .map(|threads| Ok(Row { runtime: "tflite", threads, micros: time(&dir, &LiteRtRuntime, &natives, threads)? }))
        .collect()
}

/// `track_roi`'s sorted times, in microseconds, on `dir`'s graphs through `runtime` at `threads` threads.
fn time(dir: &ModelDir, runtime: &dyn Runtime, natives: &dyn FileSource, threads: u16) -> Result<Vec<f64>, String> {
    let options = SessionOptions::new(vec![Provider::Cpu], threads);
    let graphs = dir.load(runtime, natives, &options).map_err(|e| format!("loading the graphs: {e}"))?;
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
    Ok(micros)
}

/// Prints the table of `rows` and the builds that ran them.
fn table(rows: &[Row], out: &mut dyn Write) -> Result<(), String> {
    say(out, &format!("## track_roi timing, {W}x{H}, {CALLS} calls after {WARM_UP} warm-up\n"))?;
    say(out, "| runtime | threads | mean (ms) | p50 (ms) | p95 (ms) |\n|---|---|---|---|---|")?;
    for row in rows {
        let mean = row.micros.iter().sum::<f64>() / f64::from(CALLS);
        say(
            out,
            &format!(
                "| {} | {} | {:.2} | {:.2} | {:.2} |",
                row.runtime,
                row.threads,
                mean / 1e3,
                nearest_rank(&row.micros, 50) / 1e3,
                nearest_rank(&row.micros, 95) / 1e3
            ),
        )?;
    }
    say(out, "")?;
    let ort = omuzaki_ai_ort::loaded().map_or_else(
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
    if rows.iter().any(|row| row.runtime == "onnx") {
        say(out, &format!("- ORT build: {ort}"))?;
        say(out, "- graph optimisation: ONNX Runtime's default (`omuzaki-ai-ort` sets no level)")?;
    }
    if let Some(loaded) = omuzaki_ai_litert::loaded() {
        say(
            out,
            &format!(
                "- LiteRT build: {} (LiteRT {}, XNNPACK)",
                loaded.build.id,
                omuzaki_ai_litert::builds::LITERT_VERSION
            ),
        )?;
        say(out, "- face graphs: the official face_landmarker.task (float16, version 1)")?;
    }
    say(out, &format!("- platform: {}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

/// Prints the pinned ONNX Runtime archive for this platform (the URL, SHA-256 and size to fetch and check), then the
/// official face bundle for the LiteRT run.
pub fn pins(out: &mut dyn Write) -> Result<(), String> {
    let build = builds::select_here(&[]).ok_or("no pinned ONNX Runtime build for this platform")?;
    let pin = build.pin();
    say(out, &format!("asset {}\nurl {}\nsha256 {}\nsize {}", build.asset, pin.urls.join(" "), pin.sha256, pin.size))?;
    let task = omuzaki_track::provision::task_pin();
    say(out, &format!("\nface bundle\nurl {}\nsha256 {}\nsize {}", task.urls.join(" "), task.sha256, task.size))
}
