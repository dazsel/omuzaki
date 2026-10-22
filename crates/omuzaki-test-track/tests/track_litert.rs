//! The face tracker as the app builds it (`omuzaki_track::provision`): the official `face_landmarker.task` through a
//! verified cache, unpacked, its TFLite graphs loaded through the `tflite` runtime (LiteRT with XNNPACK), a flat frame
//! finding no face and a run repeating bit for bit.
//!
//! Gated on `OMUZAKI_FACE_TASK` (a downloaded copy of the bundle), `OMUZAKI_LITERT_CACHE` and `OMUZAKI_LITERT_SHA256`
//! (the LiteRT archive of the CI run that built it, and that run's digest: there is no published pin yet, #1748); each
//! test prints a line and passes when one is unset.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::doc_markdown, clippy::cast_possible_truncation)] // test: panics fail it; product names; pixel maths in range

use std::path::PathBuf;
use std::sync::OnceLock;

use omuzaki_ai_api::NoProgress;
use omuzaki_ai_litert::LiteRtRuntime;
use omuzaki_core::time::Seconds;
use omuzaki_track::models::{ModelDir, TFLITE_FILES};
use omuzaki_track::provision::{face_models, face_tracker};
use omuzaki_track::task::TASK_FILE;
use omuzaki_track::{FaceTracker, NormRect, RgbFrame};
use sha2::Digest;
use track_support::DirSource;

mod track_support;

/// The cache folder holding the bundle under its file name, after LiteRT is loaded; `None` when the gates are unset.
fn env(test: &str) -> Option<&'static (tempfile::TempDir, DirSource)> {
    static READY: OnceLock<Option<(tempfile::TempDir, DirSource)>> = OnceLock::new();
    let ready = READY.get_or_init(|| {
        let task = std::env::var_os("OMUZAKI_FACE_TASK")?;
        let cache = PathBuf::from(std::env::var_os("OMUZAKI_LITERT_CACHE")?);
        let sha256 = std::env::var("OMUZAKI_LITERT_SHA256").ok()?;
        let archive = cache.join(omuzaki_ai_litert::builds::select_here().unwrap().asset);
        let actual = format!("{:x}", sha2::Sha256::digest(std::fs::read(&archive).unwrap()));
        assert_eq!(actual, sha256.to_lowercase(), "refusing the LiteRT archive: its SHA-256 differs");
        omuzaki_ai_litert::load_archive(&archive, &sha256).unwrap();
        // The bundle under its file name in a folder of its own: the verified cache as `DirSource` serves it.
        let folder = tempfile::tempdir().unwrap();
        std::fs::copy(task, folder.path().join(TASK_FILE)).unwrap();
        let source = DirSource(folder.path().to_path_buf());
        Some((folder, source))
    });
    if ready.is_none() {
        eprintln!("skipping {test}: set OMUZAKI_FACE_TASK, OMUZAKI_LITERT_CACHE and OMUZAKI_LITERT_SHA256");
    }
    ready.as_ref()
}

fn tracker(cache: &DirSource) -> (ModelDir, FaceTracker) {
    let models = face_models(cache, &NoProgress).unwrap();
    let tracker = face_tracker(&models, &LiteRtRuntime, cache).unwrap();
    (models, tracker)
}

const W: u32 = 640;
const H: u32 = 480;

fn frame(pixels: &[u8]) -> RgbFrame<'_> {
    RgbFrame { data: pixels, width: W, height: H, stride: (W * 3) as usize }
}

#[test]
fn the_bundle_is_fetched_unpacked_and_loaded_through_litert() {
    let Some((_, cache)) = env("the_bundle_is_fetched_unpacked_and_loaded_through_litert") else { return };
    let (models, _) = tracker(cache);
    assert_eq!(models.files(), &TFLITE_FILES);
}

#[test]
fn a_flat_grey_frame_has_no_face() {
    let Some((_, cache)) = env("a_flat_grey_frame_has_no_face") else { return };
    let (_, mut tracker) = tracker(cache);
    let grey = vec![128u8; (W * H * 3) as usize];
    for i in 0..3 {
        let (sample, _) = tracker.track(&frame(&grey), Seconds(f64::from(i) / 30.0)).unwrap();
        assert!(sample.is_none(), "frame {i}");
    }
}

#[test]
fn track_roi_is_bit_identical_across_runs() {
    let Some((_, cache)) = env("track_roi_is_bit_identical_across_runs") else { return };
    let mut pixels = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            pixels.extend([(x * 255 / (W - 1)) as u8, (y * 255 / (H - 1)) as u8, ((x + y) % 256) as u8]);
        }
    }
    let roi = NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.667, rotation: 0.0 };
    let run = || tracker(cache).1.track_roi(&frame(&pixels), roi, Seconds(0.0)).unwrap();
    let (a, b) = (run(), run());
    let bits = |s: &omuzaki_track::FaceSample| {
        s.landmarks.iter().flatten().chain(&s.blendshapes).map(|v| v.to_bits()).collect::<Vec<_>>()
    };
    assert_eq!(bits(&a), bits(&b));
    assert!(a.landmarks.iter().flatten().all(|v| v.is_finite()));
}
