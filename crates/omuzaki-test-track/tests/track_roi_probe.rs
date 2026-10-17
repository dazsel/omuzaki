//! The `track_roi_timing` example prints no path, whichever setup step fails: the model folder, or the ONNX Runtime
//! archive. A path would reach a public issue when the owner pastes the output. Both streams are checked: the table
//! goes to stdout and the error to stderr, and `main` prints exactly what `run` writes and returns.

#![allow(clippy::expect_used)]

#[path = "../examples/track_roi_timing/percentile.rs"]
mod percentile;
#[path = "../examples/track_roi_timing/probe.rs"]
mod probe;
#[path = "track_support/mod.rs"]
mod track_support;

use std::ffi::OsString;
use std::path::Path;

use probe::{Setup, pins, redact, run};
use sha2::{Digest, Sha256};

/// The files of a model folder, as `omuzaki_track::models::MODEL_FILES` names them.
const FILES: [&str; 4] = [
    "face_detector.onnx",
    "face_landmarks_detector.onnx",
    "face_blendshapes.onnx",
    "geometry_pipeline_metadata_landmarks.binarypb",
];

/// The smallest ONNX model the runtime's scan accepts: IR version 8, opset 13, a graph named `g` with no nodes. It has
/// no ports, so loading it into a slot fails, but only after the runtime archive has been asked for.
const EMPTY_ONNX: [u8; 11] = [0x08, 0x08, 0x42, 0x02, 0x10, 0x0d, 0x3a, 0x03, 0x12, 0x01, 0x67];

/// Writes a model folder whose manifest matches its files, so it opens; the graphs are empty, not real models.
fn write_models(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create the model folder");
    let mut files = serde_json::Map::new();
    for name in FILES {
        let bytes = if Path::new(name).extension().is_some_and(|e| e == "onnx") {
            EMPTY_ONNX.to_vec()
        } else {
            b"geometry".to_vec()
        };
        std::fs::write(dir.join(name), &bytes).expect("write a model file");
        let sum = format!("{:x}", Sha256::digest(&bytes));
        files.insert(name.to_owned(), serde_json::json!({ "sha256": sum }));
    }
    std::fs::write(dir.join("manifest.json"), serde_json::json!({ "files": files }).to_string()).expect("manifest");
}

/// Runs the probe and returns (what it wrote to stdout, the error it would print on stderr).
fn probe(models: &Path, cache: &Path) -> (String, String) {
    let setup = Setup { models: Some(OsString::from(models)), cache: Some(OsString::from(cache)) };
    let mut out = Vec::new();
    let error = run(&setup, &mut out).expect_err("the setup is broken, so the probe fails");
    (String::from_utf8(out).expect("stdout is text"), error)
}

fn assert_no_path(streams: &[&str], places: &[&Path]) {
    for stream in streams {
        for place in places {
            let name = place.file_name().expect("a named folder").to_string_lossy();
            assert!(
                !stream.contains(&*name) && !stream.contains(&*place.to_string_lossy()),
                "{stream:?} names {}",
                place.display()
            );
        }
    }
}

#[test]
fn missing_models_are_reported_without_the_folder() {
    let root = tempfile::tempdir().expect("temp folder");
    let models = root.path().join("models-folder-7f3a");
    let cache = root.path().join("ort-cache-9c1d");
    std::fs::create_dir(&cache).expect("cache folder");
    let (stdout, stderr) = probe(&models, &cache);
    assert!(stderr.contains("the model folder: missing or unreadable"), "{stderr}");
    assert!(stderr.contains("<folder>"), "the folder is replaced, not dropped: {stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_no_path(&[&stdout, &stderr], &[root.path(), &models, &cache]);
}

#[test]
fn a_missing_archive_is_reported_without_the_cache_folder() {
    let root = tempfile::tempdir().expect("temp folder");
    let models = root.path().join("models-folder-7f3a");
    let cache = root.path().join("ort-cache-9c1d");
    write_models(&models);
    std::fs::create_dir(&cache).expect("cache folder");
    let (stdout, stderr) = probe(&models, &cache);
    assert!(stderr.starts_with("loading the graphs:"), "the models open and the archive is what is missing: {stderr}");
    assert!(stderr.contains("<folder>"), "the archive's path was in the error, so it is replaced: {stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_no_path(&[&stdout, &stderr], &[root.path(), &models, &cache]);
}

#[test]
fn an_unset_folder_skips_and_names_only_the_variables() {
    let mut out = Vec::new();
    run(&Setup { models: None, cache: None }, &mut out).expect("a skipped probe succeeds");
    let out = String::from_utf8(out).expect("text");
    assert!(out.starts_with("skipped: set OMUZAKI_TRACK_MODELS"), "{out}");
}

#[test]
fn redact_replaces_every_place_longest_first() {
    let hidden = vec!["/work/models".to_owned(), "/work/models/cache".to_owned()];
    assert_eq!(redact("a /work/models/cache/x and /work/models/y", &hidden), "a <folder>/x and <folder>/y");
}

#[test]
fn redact_leaves_places_with_nothing_to_leak_alone() {
    let hidden = vec![String::new(), ".".to_owned(), "/".to_owned(), "./".to_owned()];
    assert_eq!(redact("0.5 ms in ./a/b", &hidden), "0.5 ms in ./a/b");
}

#[test]
fn the_pins_flag_prints_the_archive_to_fetch_and_no_path() {
    let mut out = Vec::new();
    match pins(&mut out) {
        Ok(()) => {
            let out = String::from_utf8(out).expect("text");
            assert!(out.starts_with("asset ") && out.contains("\nsha256 "), "{out}");
        }
        Err(message) => assert!(message.starts_with("no pinned ONNX Runtime build"), "{message}"),
    }
}
