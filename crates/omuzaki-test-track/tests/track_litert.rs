//! The face tracker as the app builds it (`omuzaki_track::provision`): the official `face_landmarker.task` unpacked
//! into a models folder (what the release ships), the folder opened and its TFLite graphs loaded through the `tflite`
//! runtime (LiteRT with XNNPACK), found among the linked runtimes; a flat frame finding no face and a run repeating bit
//! for bit.
//!
//! Gated on `OMUZAKI_FACE_TASK` (a downloaded copy of the bundle), `OMUZAKI_LITERT_CACHE` and `OMUZAKI_LITERT_SHA256`
//! (the LiteRT archive of the CI run that built it, and that run's digest: there is no published pin yet, #1748); each
//! test prints a line and passes when one is unset.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::doc_markdown, clippy::cast_possible_truncation)] // test: panics fail it; product names; pixel maths in range

use std::path::PathBuf;
use std::sync::OnceLock;

use omuzaki_core::time::Seconds;
use omuzaki_track::models::{ModelDir, TFLITE_FILES};
use omuzaki_track::provision::{face_models, face_tracker, runtime_for};
use omuzaki_track::task::{TASK_FILE, unpack};
use omuzaki_track::{FaceTracker, NormRect, RgbFrame};
use sha2::Digest;
use track_support::DirSource;

mod track_support;

/// The scratch folder, the unpacked models folder in it and the LiteRT archive's source, after LiteRT is loaded;
/// `None` when the gates are unset.
type Env = (tempfile::TempDir, PathBuf, DirSource);

fn env(test: &str) -> Option<&'static Env> {
    static READY: OnceLock<Option<Env>> = OnceLock::new();
    let ready = READY.get_or_init(|| {
        let task = std::env::var_os("OMUZAKI_FACE_TASK")?;
        let cache = PathBuf::from(std::env::var_os("OMUZAKI_LITERT_CACHE")?);
        let sha256 = std::env::var("OMUZAKI_LITERT_SHA256").ok()?;
        omuzaki_ai_litert::link();
        let archive = cache.join(omuzaki_ai_litert::builds::select_here().unwrap().asset);
        let actual = format!("{:x}", sha2::Sha256::digest(std::fs::read(&archive).unwrap()));
        assert_eq!(actual, sha256.to_lowercase(), "refusing the LiteRT archive: its SHA-256 differs");
        omuzaki_ai_litert::load_archive(&archive, &sha256).unwrap();
        // The bundle unpacked once, as the release build does; the app then opens only the folder.
        let scratch = tempfile::tempdir().unwrap();
        let copy = scratch.path().join(TASK_FILE);
        std::fs::copy(task, &copy).unwrap();
        let models = unpack(&copy).unwrap().path().to_path_buf();
        Some((scratch, models, DirSource(cache)))
    });
    if ready.is_none() {
        eprintln!("skipping {test}: set OMUZAKI_FACE_TASK, OMUZAKI_LITERT_CACHE and OMUZAKI_LITERT_SHA256");
    }
    ready.as_ref()
}

fn tracker((_, folder, natives): &Env) -> (ModelDir, FaceTracker) {
    let models = face_models(folder).unwrap();
    assert_eq!(runtime_for(&models), "tflite");
    let tracker = face_tracker(&models, natives).unwrap();
    (models, tracker)
}

const W: u32 = 640;
const H: u32 = 480;

fn frame(pixels: &[u8]) -> RgbFrame<'_> {
    RgbFrame { data: pixels, width: W, height: H, stride: (W * 3) as usize }
}

#[test]
fn the_shipped_folder_opens_and_loads_through_litert() {
    let Some(env) = env("the_shipped_folder_opens_and_loads_through_litert") else { return };
    let (models, _) = tracker(env);
    assert_eq!(models.files(), &TFLITE_FILES);
}

#[test]
fn a_flat_grey_frame_has_no_face() {
    let Some(env) = env("a_flat_grey_frame_has_no_face") else { return };
    let (_, mut tracker) = tracker(env);
    let grey = vec![128u8; (W * H * 3) as usize];
    for i in 0..3 {
        let (sample, _) = tracker.track(&frame(&grey), Seconds(f64::from(i) / 30.0)).unwrap();
        assert!(sample.is_none(), "frame {i}");
    }
}

#[test]
fn track_roi_is_bit_identical_across_runs() {
    let Some(env) = env("track_roi_is_bit_identical_across_runs") else { return };
    let mut pixels = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            pixels.extend([(x * 255 / (W - 1)) as u8, (y * 255 / (H - 1)) as u8, ((x + y) % 256) as u8]);
        }
    }
    let roi = NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.667, rotation: 0.0 };
    let run = || tracker(env).1.track_roi(&frame(&pixels), roi, Seconds(0.0)).unwrap();
    let (a, b) = (run(), run());
    let bits = |s: &omuzaki_track::FaceSample| {
        s.landmarks.iter().flatten().chain(&s.blendshapes).map(|v| v.to_bits()).collect::<Vec<_>>()
    };
    assert_eq!(bits(&a), bits(&b));
    assert!(a.landmarks.iter().flatten().all(|v| v.is_finite()));
}
