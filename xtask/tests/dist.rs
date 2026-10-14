//! `cargo xtask dist`: the zip layout, determinism, and what it refuses. Every input is a stand-in file; no real model or
//! ONNX Runtime archive is needed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use xtask::dist::{DistInputs, verify_models, write_zip};
use zip::ZipArchive;
use zip::write::SimpleFileOptions;

const MODEL_FILES: [&str; 4] = [
    "face_detector.onnx",
    "face_landmarks_detector.onnx",
    "face_blendshapes.onnx",
    "geometry_pipeline_metadata_landmarks.binarypb",
];

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes).iter().fold(String::new(), |mut text, b| {
        write!(text, "{b:02x}").unwrap();
        text
    })
}

/// The three source networks the converter unpacks from the `.task` bundle; they stay out of the zip.
const SOURCE_FILES: [&str; 3] = ["face_detector.tflite", "face_landmarks_detector.tflite", "face_blendshapes.tflite"];

/// A models folder of stand-in files plus a `manifest.json` shaped like the converter's
/// (`tools/models/convert_mediapipe_face.py`): the provenance keys, and a `files` table that lists the source `.tflite`
/// files as well as the shipped ones.
fn models_dir(root: &Path) -> PathBuf {
    let dir = root.join("models");
    fs::create_dir_all(&dir).unwrap();
    let mut files = serde_json::Map::new();
    for name in SOURCE_FILES.into_iter().chain(MODEL_FILES) {
        let bytes = format!("stand-in for {name}").into_bytes();
        fs::write(dir.join(name), &bytes).unwrap();
        let from = if name.ends_with(".onnx") { "tf2onnx" } else { "bundle" };
        files.insert(name.to_owned(), serde_json::json!({ "sha256": hex(&bytes), "bytes": bytes.len(), "from": from }));
    }
    let manifest = serde_json::json!({
        "source_url": "https://example.invalid/face_landmarker.task",
        "bundle": { "file": "face_landmarker.task", "sha256": hex(b"bundle"), "bytes": 6 },
        "tools": { "tensorflow": "2.16.2", "tf2onnx": "1.16.1", "onnx": "1.16.2", "onnxruntime": "1.26.0" },
        "files": files,
    });
    fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
    dir
}

/// A stand-in for Microsoft's archive, with the given member names (under one top folder, like the real one).
fn ort_zip(root: &Path, members: &[&str]) -> PathBuf {
    let path = root.join("ort.zip");
    let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
    for member in members {
        zip.start_file(format!("onnxruntime-win-x64-1.28.3/{member}"), SimpleFileOptions::default()).unwrap();
        zip.write_all(format!("stand-in for {member}").as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    path
}

const ORT_MEMBERS: [&str; 3] = ["lib/onnxruntime.dll", "LICENSE", "ThirdPartyNotices.txt"];

fn inputs(root: &Path, models: bool, ort: bool) -> DistInputs {
    fs::write(root.join("omuzaki.exe"), b"MZ stand-in").unwrap();
    fs::write(root.join("notices.txt"), b"third-party notices").unwrap();
    DistInputs {
        exe: root.join("omuzaki.exe"),
        models: models.then(|| models_dir(root)),
        ort_archive: ort.then(|| ort_zip(root, &ORT_MEMBERS)),
        notices: root.join("notices.txt"),
        version: "1.2.3".to_owned(),
    }
}

fn names(zip: &Path) -> Vec<String> {
    let mut archive = ZipArchive::new(File::open(zip).unwrap()).unwrap();
    (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned()).collect()
}

fn member(zip: &Path, name: &str) -> String {
    let mut archive = ZipArchive::new(File::open(zip).unwrap()).unwrap();
    let mut text = String::new();
    archive.by_name(name).unwrap().read_to_string(&mut text).unwrap();
    text
}

#[test]
fn the_layout_is_exact_without_the_ort_archive() {
    let root = TempDir::new().unwrap();
    let zip = write_zip(&inputs(root.path(), true, false), &root.path().join("out")).unwrap();
    assert_eq!(zip.file_name().unwrap(), "omuzaki-1.2.3-windows-x64.zip");
    assert_eq!(
        names(&zip),
        [
            "README.txt",
            "models/mediapipe-face/face_blendshapes.onnx",
            "models/mediapipe-face/face_detector.onnx",
            "models/mediapipe-face/face_landmarks_detector.onnx",
            "models/mediapipe-face/geometry_pipeline_metadata_landmarks.binarypb",
            "models/mediapipe-face/manifest.json",
            "notices/Apache-2.0.txt",
            "notices/MODELS.md",
            "notices/THIRD-PARTY-NOTICES.txt",
            "omuzaki.exe",
        ]
    );
    assert_eq!(member(&zip, "omuzaki.exe"), "MZ stand-in");
    assert_eq!(member(&zip, "notices/THIRD-PARTY-NOTICES.txt"), "third-party notices");
}

#[test]
fn the_layout_adds_the_runtime_and_its_notices_with_the_ort_archive() {
    let root = TempDir::new().unwrap();
    let zip = write_zip(&inputs(root.path(), true, true), &root.path().join("out")).unwrap();
    let listed = names(&zip);
    for extra in ["onnxruntime.dll", "notices/onnxruntime-LICENSE.txt", "notices/onnxruntime-ThirdPartyNotices.txt"] {
        assert!(listed.contains(&extra.to_owned()), "{extra} missing from {listed:?}");
    }
    assert_eq!(listed.len(), 13);
    assert_eq!(member(&zip, "onnxruntime.dll"), "stand-in for lib/onnxruntime.dll");
    assert_eq!(member(&zip, "notices/onnxruntime-LICENSE.txt"), "stand-in for LICENSE");
}

/// Unpacks the zip's `models/mediapipe-face` folder into `dest`.
fn extract_models(zip: &Path, dest: &Path) {
    let mut archive = ZipArchive::new(File::open(zip).unwrap()).unwrap();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().unwrap().into_owned();
        if let Some(file) = name.strip_prefix("models/mediapipe-face/") {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            fs::write(dest.join(file), bytes).unwrap();
        }
    }
}

#[test]
fn the_shipped_models_verify_after_extraction_and_keep_their_provenance() {
    let root = TempDir::new().unwrap();
    let zip = write_zip(&inputs(root.path(), true, false), &root.path().join("out")).unwrap();
    let unpacked = root.path().join("unpacked");
    fs::create_dir(&unpacked).unwrap();
    extract_models(&zip, &unpacked);
    verify_models(&unpacked).unwrap();

    let shipped: serde_json::Value =
        serde_json::from_str(&member(&zip, "models/mediapipe-face/manifest.json")).unwrap();
    let listed: Vec<&str> = shipped["files"].as_object().unwrap().keys().map(String::as_str).collect();
    let mut expected = MODEL_FILES.to_vec();
    expected.sort_unstable();
    assert_eq!(listed, expected, "the manifest lists exactly the shipped files");
    let source: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.path().join("models/manifest.json")).unwrap()).unwrap();
    for key in ["source_url", "bundle", "tools"] {
        assert_eq!(shipped[key], source[key], "{key} is kept");
    }
    assert_eq!(
        shipped["files"]["face_detector.onnx"], source["files"]["face_detector.onnx"],
        "per-file entries are kept"
    );
}

#[test]
fn two_runs_give_identical_bytes() {
    let root = TempDir::new().unwrap();
    let inputs = inputs(root.path(), true, true);
    let first = fs::read(write_zip(&inputs, &root.path().join("a")).unwrap()).unwrap();
    // The second run starts later and from other files with the same content.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let touched = root.path().join("omuzaki-copy.exe");
    fs::copy(&inputs.exe, &touched).unwrap();
    let second = fs::read(write_zip(&DistInputs { exe: touched, ..inputs }, &root.path().join("b")).unwrap()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn entries_have_fixed_timestamps_and_permissions() {
    let root = TempDir::new().unwrap();
    let zip = write_zip(&inputs(root.path(), true, false), &root.path().join("out")).unwrap();
    let mut archive = ZipArchive::new(File::open(zip).unwrap()).unwrap();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).unwrap();
        let time = entry.last_modified().unwrap();
        assert_eq!(
            (time.year(), time.month(), time.day(), time.hour(), time.minute(), time.second()),
            (1980, 1, 1, 0, 0, 0)
        );
        assert_eq!(entry.unix_mode(), Some(0o100_644), "{:?}", entry.name());
    }
}

#[test]
fn a_model_whose_hash_does_not_match_is_refused() {
    let root = TempDir::new().unwrap();
    let inputs = inputs(root.path(), true, false);
    let models = inputs.models.clone().unwrap();
    fs::write(models.join("face_detector.onnx"), b"tampered").unwrap();
    let error = verify_models(&models).unwrap_err().to_string();
    assert!(error.contains("face_detector.onnx") && error.contains("SHA-256"), "{error}");
    assert!(write_zip(&inputs, &root.path().join("out")).is_err());
    assert!(!root.path().join("out").exists(), "a refused run writes nothing");
}

#[test]
fn a_models_folder_missing_a_listed_file_is_refused() {
    let root = TempDir::new().unwrap();
    let inputs = inputs(root.path(), true, false);
    let models = inputs.models.clone().unwrap();
    fs::remove_file(models.join("face_blendshapes.onnx")).unwrap();
    let error = verify_models(&models).unwrap_err().to_string();
    assert!(error.contains("face_blendshapes.onnx") && error.contains("missing"), "{error}");
    assert!(write_zip(&inputs, &root.path().join("out")).is_err());
}

#[test]
fn a_manifest_that_omits_a_required_model_is_refused() {
    let root = TempDir::new().unwrap();
    let models = models_dir(root.path());
    fs::write(models.join("manifest.json"), r#"{"files":{}}"#).unwrap();
    assert!(verify_models(&models).unwrap_err().to_string().contains("does not list"));
}

#[test]
fn a_manifest_name_that_leaves_the_folder_is_refused() {
    let root = TempDir::new().unwrap();
    let models = models_dir(root.path());
    fs::write(models.join("manifest.json"), r#"{"files":{"../outside":"00"}}"#).unwrap();
    assert!(verify_models(&models).unwrap_err().to_string().contains("unsafe"));
}

#[test]
fn an_ort_zip_without_license_is_refused() {
    let root = TempDir::new().unwrap();
    let mut inputs = inputs(root.path(), false, false);
    inputs.ort_archive = Some(ort_zip(root.path(), &["lib/onnxruntime.dll", "ThirdPartyNotices.txt"]));
    let error = write_zip(&inputs, &root.path().join("out")).unwrap_err().to_string();
    assert!(error.contains("LICENSE"), "{error}");
}

#[test]
fn no_models_leaves_out_models_and_the_readme_says_so() {
    let root = TempDir::new().unwrap();
    let zip = write_zip(&inputs(root.path(), false, false), &root.path().join("out")).unwrap();
    assert!(names(&zip).iter().all(|name| !name.starts_with("models/")), "{:?}", names(&zip));
    assert!(member(&zip, "README.txt").contains("does NOT include the face models"));
    let with = write_zip(&inputs(root.path(), true, false), &root.path().join("out2")).unwrap();
    assert!(!member(&with, "README.txt").contains("does NOT include"));
}

fn xtask(args: &[&str]) -> std::process::Output {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    Command::new(env!("CARGO_BIN_EXE_xtask")).arg("dist").args(args).current_dir(root).output().unwrap()
}

#[test]
fn the_command_prints_the_zip_path() {
    let root = TempDir::new().unwrap();
    let inputs = inputs(root.path(), false, false);
    let out = root.path().join("out");
    let run = xtask(&[
        "--no-models",
        "--exe",
        inputs.exe.to_str().unwrap(),
        "--notices",
        inputs.notices.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let printed = String::from_utf8(run.stdout).unwrap();
    assert!(Path::new(printed.trim()).is_file(), "{printed}");
    assert!(printed.trim().ends_with("-windows-x64.zip"));
}

#[test]
fn the_command_exits_2_with_one_line_when_it_refuses() {
    let root = TempDir::new().unwrap();
    let inputs = inputs(root.path(), false, false);
    let ort = ort_zip(root.path(), &ORT_MEMBERS);
    let run = xtask(&[
        "--no-models",
        "--exe",
        inputs.exe.to_str().unwrap(),
        "--notices",
        inputs.notices.to_str().unwrap(),
        "--ort-archive",
        ort.to_str().unwrap(),
    ]);
    assert_eq!(run.status.code(), Some(2));
    let stderr = String::from_utf8(run.stderr).unwrap();
    assert_eq!(stderr.trim().lines().count(), 1, "{stderr}");
    assert!(stderr.contains("pinned build"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "");
}

#[test]
fn the_command_needs_models_or_no_models() {
    let run = xtask(&["--exe", "x"]);
    assert_eq!(run.status.code(), Some(2));
}

#[test]
fn the_pinned_ort_hash_is_the_one_omuzaki_ai_ort_pins() {
    let builds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/omuzaki-ai-ort/src/builds.rs");
    let source = fs::read_to_string(builds).unwrap();
    let dist = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dist.rs")).unwrap();
    let pin = dist.split("ORT_ARCHIVE_SHA256: &str = \"").nth(1).unwrap().split('"').next().unwrap();
    assert!(source.contains(&format!("asset: \"onnxruntime-win-x64-1.28.3.zip\",\n        archive: Archive::Zip,\n        library: \"onnxruntime.dll\",\n        sha256: hash(\"{pin}\")")), "builds.rs pins another win-x64 archive than dist.rs");
}
