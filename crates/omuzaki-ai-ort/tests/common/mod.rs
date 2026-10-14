//! Shared by the integration tests: a file source over a prepared cache, and the gate for tests that need it.
//!
//! `OMUZAKI_ORT_CACHE` names a folder holding the pinned ONNX Runtime archive for this machine under its release file name
//! (`onnxruntime-linux-x64-1.28.3.tgz`). Tests that need it skip with a message when it is unset, so the default gate
//! stays offline. The source verifies size and SHA-256 like the real cache does, and never downloads.
#![allow(dead_code, clippy::unwrap_used)] // shared by several test binaries; panics are test failures

use std::path::{Path, PathBuf};

use omuzaki_ai_api::{
    FetchError, FileSource, LocalFile, ModelId, ModelRef, ModelSource, PinnedFile, Progress, Sha256, Version,
};
use sha2::{Digest, Sha256 as Hasher};

/// Verified archives from a folder.
pub struct DirSource(pub PathBuf);

impl FileSource for DirSource {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        let asset = pin.urls[0].rsplit('/').next().unwrap();
        let path = self.0.join(asset);
        let io = |e: std::io::Error| FetchError::Io { path: path.display().to_string(), reason: e.to_string() };
        let bytes = std::fs::read(&path).map_err(io)?;
        if bytes.len() as u64 != pin.size {
            return Err(FetchError::Size { url: asset.to_owned(), expected: pin.size, actual: bytes.len() as u64 });
        }
        let actual = Sha256(Hasher::digest(&bytes).into());
        if actual != pin.sha256 {
            return Err(FetchError::Hash { url: asset.to_owned(), expected: pin.sha256, actual });
        }
        Ok(path)
    }
}

/// The prepared cache, or `None` (with a message on stderr) when `OMUZAKI_ORT_CACHE` is unset.
pub fn cache(test: &str) -> Option<DirSource> {
    let dir = std::env::var_os("OMUZAKI_ORT_CACHE").map(|dir| DirSource(PathBuf::from(dir)));
    if dir.is_none() {
        eprintln!("skipping {test}: set OMUZAKI_ORT_CACHE to a folder holding the pinned ONNX Runtime archive");
    }
    dir
}

/// A source that must never be asked for a file: for tests that fail before the library is needed.
pub struct NoNetwork;

impl FileSource for NoNetwork {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        panic!("unexpected fetch of {:?}", pin.urls);
    }
}

/// A model reference for tests: the digest is the SHA-256 of nothing in particular.
pub fn model_ref() -> ModelRef {
    ModelRef {
        id: ModelId::new("test-model").unwrap(),
        version: "1.0.0".parse::<Version>().unwrap(),
        digest: Sha256([7; 32]),
    }
}

/// One model file, by its manifest name.
pub fn local(name: &str, path: &Path) -> LocalFile {
    LocalFile { name: name.to_owned(), path: path.to_path_buf() }
}

/// A fixture model in `tests/fixtures`.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Runs `body` with a `ModelSource` over `files`.
pub fn with_source<R>(
    files: &[LocalFile],
    stages: &[omuzaki_ai_api::Stage],
    natives: &dyn FileSource,
    body: impl FnOnce(&ModelSource<'_>) -> R,
) -> R {
    let model = model_ref();
    body(&ModelSource { model: &model, files, stages, natives })
}
