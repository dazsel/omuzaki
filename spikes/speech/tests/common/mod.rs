//! Where the integration tests find models and audio. Heavy files live outside git (docs/decisions/0003), so every
//! test that needs one skips with a message when it is absent instead of failing a checkout that never downloaded it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use speech_spike::vad::{SileroVad, load_runtime};

/// The assets folder: `OMUZAKI_ASSETS`, or `assets-local` in the repository root (this spike is its own workspace, so the
/// lookup that `omuzaki_core::assets` does for the main workspace is repeated here).
pub fn assets_dir() -> PathBuf {
    std::env::var_os("OMUZAKI_ASSETS")
        .filter(|v| !v.is_empty())
        .map_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets-local"), PathBuf::from)
}

/// The model directory: `OMUZAKI_SPEECH_MODELS`, or `models/speech` under the assets folder.
pub fn models_dir() -> PathBuf {
    std::env::var_os("OMUZAKI_SPEECH_MODELS").map_or_else(|| assets_dir().join("models/speech"), PathBuf::from)
}

/// The speech clips: `OMUZAKI_SPEECH_DATA`, or `datasets/speech-pd` under the assets folder.
pub fn data_dir() -> PathBuf {
    std::env::var_os("OMUZAKI_SPEECH_DATA").map_or_else(|| assets_dir().join("datasets/speech-pd"), PathBuf::from)
}

/// The ONNX Runtime library: `ORT_DYLIB_PATH`, or the copy unpacked next to the models.
fn runtime_path() -> PathBuf {
    std::env::var_os("ORT_DYLIB_PATH")
        .map_or_else(|| models_dir().join("onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so"), PathBuf::from)
}

/// Returns `path` if it exists, else prints why the test is skipping and returns `None`.
pub fn require(path: &Path) -> Option<&Path> {
    if path.exists() {
        Some(path)
    } else {
        eprintln!("SKIPPED: {} not found (see spikes/speech tests/common/mod.rs for the env vars)", path.display());
        None
    }
}

/// A fresh VAD session, or `None` (skip) when the runtime or the model is missing.
pub fn vad() -> Option<SileroVad> {
    static LOADED: OnceLock<bool> = OnceLock::new();
    let runtime = runtime_path();
    let model = models_dir().join("silero_vad.onnx");
    require(&runtime)?;
    require(&model)?;
    let ok = *LOADED.get_or_init(|| load_runtime(&runtime).is_ok());
    ok.then(|| SileroVad::new(&model).ok()).flatten()
}
