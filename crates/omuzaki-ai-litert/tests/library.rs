//! Gated on the real archive and models: the pinned library loads with its version and ABI checked, and the scan
//! accepts the three face graphs. Each test skips with a message without `OMUZAKI_LITERT_CACHE` (and
//! `OMUZAKI_LITERT_SHA256`) or `OMUZAKI_TRACK_MODELS`.
#![allow(clippy::doc_markdown)] // product names are prose

mod common;

use std::time::Instant;

use omuzaki_ai_litert::builds::select_here;
use omuzaki_ai_litert::scan::scan_model;
use omuzaki_ai_litert::{load_archive, loaded};

/// How many scans the landmark graph's timing averages.
const RUNS: u32 = 200;

#[test]
fn the_pinned_library_loads_once_with_its_version_and_abi_checked() {
    let Some((archive, sha256)) = common::litert_archive("the library test") else { return };
    let first = load_archive(&archive, &sha256).unwrap();
    assert_eq!(first.build, select_here().unwrap());
    // Once per process: a second call, or a lookup, gives the same library.
    assert_eq!(load_archive(&archive, &sha256).unwrap().build, first.build);
    assert!(loaded().is_some());
}

#[test]
fn the_three_face_graphs_pass_the_scan_and_the_landmark_scan_is_timed() {
    let Some(dir) = common::face_models("the face graph scan") else { return };
    for name in ["face_detector", "face_landmarks_detector", "face_blendshapes"] {
        let bytes = std::fs::read(dir.join(format!("{name}.tflite"))).unwrap();
        let scan = scan_model(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!scan.signature.inputs.is_empty() && !scan.signature.outputs.is_empty(), "{name}");
    }
    let landmarks = std::fs::read(dir.join("face_landmarks_detector.tflite")).unwrap();
    let start = Instant::now();
    for _ in 0..RUNS {
        scan_model(&landmarks).unwrap();
    }
    eprintln!(
        "scan of the landmark graph ({} bytes): {:?} per scan, mean of {RUNS}",
        landmarks.len(),
        start.elapsed() / RUNS
    );
}
