//! Shared by the integration tests: synthetic models, a model source over files, and the gates for tests that need
//! the LiteRT archive or the face graphs.
//!
//! `OMUZAKI_LITERT_CACHE` names a folder holding this platform's archive under its asset name from the build table,
//! and `OMUZAKI_LITERT_SHA256` the SHA-256 the CI run that built it reported (`runtime-litert.yml`'s `collect`). There is
//! no published pin yet (#1748), so that digest is the archive's trust root: an archive without one is refused.
#![allow(dead_code, clippy::unwrap_used)] // shared by several test binaries; panics are test failures

pub mod model;

use std::path::{Path, PathBuf};

use omuzaki_ai_api::{FetchError, FileSource, LocalFile, ModelId, ModelRef, ModelSource, PinnedFile, Progress, Sha256};
use omuzaki_ai_litert::builds::select_here;
use sha2::{Digest, Sha256 as Hasher};

/// This platform's archive from `OMUZAKI_LITERT_CACHE`, checked against `OMUZAKI_LITERT_SHA256`, with that digest; or
/// `None` (with a message) when the cache is unset.
///
/// # Panics
/// When the cache is set but the digest is missing or differs: such an archive is never loaded.
pub fn litert_archive(test: &str) -> Option<(PathBuf, String)> {
    let Some(dir) = std::env::var_os("OMUZAKI_LITERT_CACHE") else {
        eprintln!("skipping {test}: set OMUZAKI_LITERT_CACHE to a folder holding the LiteRT archive");
        return None;
    };
    let expected = std::env::var("OMUZAKI_LITERT_SHA256").unwrap_or_default();
    assert!(expected.len() == 64, "refusing the archive: OMUZAKI_LITERT_SHA256 must be the build job's SHA-256");
    let path = PathBuf::from(dir).join(select_here().unwrap().asset);
    let actual = format!("{:x}", Hasher::digest(std::fs::read(&path).unwrap()));
    assert_eq!(actual, expected.to_lowercase(), "refusing {}: its SHA-256 differs", path.display());
    Some((path, actual))
}

/// The face model folder `tools/models/mediapipe-face.sh` writes, from `OMUZAKI_TRACK_MODELS`, or `None` (with a
/// message) when it is unset.
pub fn face_models(test: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("OMUZAKI_TRACK_MODELS").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("skipping {test}: set OMUZAKI_TRACK_MODELS to the face model folder");
    }
    dir
}

/// A source that must never be asked for a file: the scan needs no native library.
pub struct NoNetwork;

impl FileSource for NoNetwork {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        panic!("unexpected fetch of {:?}", pin.urls);
    }
}

/// Runs `body` with a `ModelSource` whose main file is `path`.
pub fn with_model<R>(path: &Path, stages: &[omuzaki_ai_api::Stage], body: impl FnOnce(&ModelSource<'_>) -> R) -> R {
    let model = ModelRef {
        id: ModelId::new("test-model").unwrap(),
        version: "1.0.0".parse().unwrap(),
        digest: Sha256([7; 32]),
    };
    let files = [LocalFile { name: "model.tflite".to_owned(), path: path.to_path_buf() }];
    body(&ModelSource { model: &model, files: &files, stages, natives: &NoNetwork })
}

/// Loads this platform's checked archive for `test`, or returns `false` (with a message) when there is none.
pub fn library(test: &str) -> bool {
    let Some((archive, sha256)) = litert_archive(test) else { return false };
    omuzaki_ai_litert::load_archive(&archive, &sha256).unwrap();
    true
}

/// Loads the model file at `path` as a session presenting `slot`, with `threads` threads and model names as slot names.
pub fn load_file(path: &Path, slot: &omuzaki_ai_api::Signature, threads: u16) -> omuzaki_ai_litert::LiteRtSession {
    let options = omuzaki_ai_api::SessionOptions::new(vec![omuzaki_ai_api::Provider::Cpu], threads);
    with_model(path, &[], |source| {
        omuzaki_ai_litert::LiteRtRuntime.load_session(source, slot, &std::collections::BTreeMap::new(), &options)
    })
    .unwrap()
}

/// Loads the synthetic `model` (written to a scratch file) as [`load_file`] does.
pub fn load_synthetic(
    model: &model::Table,
    slot: &omuzaki_ai_api::Signature,
    threads: u16,
) -> omuzaki_ai_litert::LiteRtSession {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.tflite");
    std::fs::write(&path, model::bytes(model)).unwrap();
    load_file(&path, slot, threads)
}

/// The `ADD` graph's slot: inputs `a` and `b`, output `out`, each `FLOAT32` with `dims`.
pub fn add_slot(dims: &[omuzaki_ai_api::Dim]) -> omuzaki_ai_api::Signature {
    let spec = |name: &str| omuzaki_ai_api::TensorSpec::new(name, omuzaki_ai_api::DType::F32, dims.to_vec());
    omuzaki_ai_api::Signature { inputs: vec![spec("a"), spec("b")], outputs: vec![spec("out")] }
}
