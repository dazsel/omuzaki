//! What the speech model tests and `examples/speech_host_bench.rs` share: a file source over the prepared folders, and
//! the registry the app would build over it, with the speech models installed.
//!
//! - `OMUZAKI_SPEECH_MODELS`: `silero_vad.onnx` and the ggml files, under their release names;
//! - `OMUZAKI_ORT_CACHE`: the pinned ONNX Runtime archive (`onnxruntime-linux-x64-1.28.3.tgz` on Linux);
//! - `OMUZAKI_SPEECH_DATA`: the LibriVox MP3s the smoke tests fetch (`fables_03_05_aesop_64kb.mp3`, ...) and, for the
//!   golden comparison, `aesop-52-farmer-and-his-sons.wav` (16 kHz mono).
//!
//! A test skips with a message when one is unset. Nothing is downloaded: the source serves a pinned file only from
//! these folders, by the last part of its URL, after checking its size and SHA-256.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::doc_markdown)] // shared by test binaries; a panic is a failure

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_ai::{Registry, RegistrySettings};
use omuzaki_ai_api::{
    FetchError, FileSource, ModelRef, ModelRegistry, NoProgress, PinnedFile, Progress, Sha256, SlotRef, Version,
};
use sha2::Digest as _;

/// Verified files from several folders; never downloads.
pub struct Folders(pub Vec<PathBuf>);

impl FileSource for Folders {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        let name = pin.urls.first().and_then(|url| url.rsplit('/').next()).unwrap_or_default();
        let Some(path) = self.0.iter().map(|dir| dir.join(name)).find(|p| p.is_file()) else {
            return Err(FetchError::Io { path: name.to_owned(), reason: "not in the prepared folders".into() });
        };
        let io = |e: std::io::Error| FetchError::Io { path: path.display().to_string(), reason: e.to_string() };
        let size = std::fs::metadata(&path).map_err(io)?.len();
        if size != pin.size {
            return Err(FetchError::Size { url: name.to_owned(), expected: pin.size, actual: size });
        }
        let mut file = std::fs::File::open(&path).map_err(io)?;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = file.read(&mut buffer).map_err(io)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = Sha256(hasher.finalize().into());
        if actual != pin.sha256 {
            return Err(FetchError::Hash { url: name.to_owned(), expected: pin.sha256, actual });
        }
        Ok(path)
    }
}

/// A folder from `var`, or `None` with a message.
pub fn folder(var: &str, what: &str) -> Option<PathBuf> {
    let dir = std::env::var_os(var).map(PathBuf::from);
    if dir.is_none() {
        eprintln!("SKIPPED: set {var} to {what}");
    }
    dir
}

/// The three folders, or `None` (the test skips).
pub fn folders() -> Option<Folders> {
    let models = folder("OMUZAKI_SPEECH_MODELS", "the folder holding silero_vad.onnx and the ggml files")?;
    let ort = folder("OMUZAKI_ORT_CACHE", "the folder holding the pinned ONNX Runtime archive")?;
    let data = folder("OMUZAKI_SPEECH_DATA", "the folder holding the LibriVox clips")?;
    Some(Folders(vec![models, ort, data]))
}

/// A registry over the folders in a fresh data folder, as the app builds it.
pub fn registry(files: Folders, data_dir: &Path) -> Registry {
    omuzaki_ai_ort::link();
    omuzaki_speech::link();
    Registry::new(data_dir, Arc::new(files), Version::new(0, 1, 0), RegistrySettings::default()).unwrap()
}

/// The shipped model `id` in `slot`.
pub fn model(registry: &Registry, slot: &SlotRef, id: &str) -> ModelRef {
    registry
        .models(Some(slot))
        .into_iter()
        .find(|m| m.model.id.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is not registered for {slot}"))
        .model
}

/// Installs `id` (download from the folders, verify, smoke test) and returns it.
pub fn install(registry: &Registry, slot: &SlotRef, id: &str) -> ModelRef {
    let model = model(registry, slot, id);
    registry.install(&model, &NoProgress).unwrap_or_else(|e| panic!("installing {id}: {e}"));
    model
}

/// A 16 kHz mono WAV as `f32`.
pub fn read_wav(path: &Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    assert_eq!((spec.sample_rate, spec.channels), (16_000, 1), "{}", path.display());
    let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
    reader.samples::<i32>().map(|s| s.unwrap() as f32 * scale).collect()
}
