//! The face tracker made ready for the app: the face models the release ships, loaded through the runtime they are
//! made for, as the [`FaceTracker`] a [`WebcamTracker`](crate::webcam::WebcamTracker) runs.
//!
//! ```text
//! face_models(<install>/models/mediapipe-face)  ->  face_tracker  ->  WebcamTracker::new(camera, tracker, ..)
//! ```
//!
//! The models come with the app, never from Google at run time: the licence record of `face_landmarker.task`
//! (`docs/licensing/deps/mediapipe-face-landmarker.toml`) says the bundle is fetched at build time only and releases
//! use our own copy. The folder holds either the unchanged TFLite graphs (for the `tflite` runtime, LiteRT, which
//! `docs/research/2026-10-track-face-budget.md` measured at twice ONNX Runtime's speed) or the ONNX conversions (for
//! the `onnx` runtime); [`face_models`] tells which, and [`face_tracker`] finds that runtime among the ones linked into
//! the app. Both are cold: call them off the UI thread.

use std::path::Path;

use omuzaki_ai_api::{FileSource, Provider, RuntimeId, SessionOptions, find_runtime};

use crate::models::{ModelDir, ModelError, TFLITE_FILES};
use crate::{FaceTracker, FaceTrackerConfig};

/// Intra-op threads per graph: two cut `track_roi` by a quarter against one, for about half a core at 30 fps
/// (`docs/research/2026-10-track-face-budget.md`).
pub const FACE_THREADS: u16 = 2;

/// The face models in `dir`, checked file by file: the unchanged TFLite graphs when the folder holds them (each
/// against its pinned SHA-256), the ONNX conversions otherwise (against the folder's `manifest.json`).
///
/// # Errors
/// [`ModelDir::open_tflite`]'s or [`ModelDir::open`]'s.
pub fn face_models(dir: &Path) -> Result<ModelDir, ModelError> {
    if dir.join(TFLITE_FILES[1]).is_file() { ModelDir::open_tflite(dir) } else { ModelDir::open(dir) }
}

/// The runtime `models` are made for: `tflite` for the TFLite graphs, `onnx` for the conversions.
pub fn runtime_for(models: &ModelDir) -> &'static str {
    if models.files() == &TFLITE_FILES { "tflite" } else { "onnx" }
}

/// The tracker over `models`, their graphs loaded through their runtime ([`runtime_for`], found among the runtimes
/// linked into the app) on the CPU at [`FACE_THREADS`] threads, in video mode with MediaPipe's thresholds. `natives`
/// is where a runtime gets its pinned native library.
///
/// # Errors
/// [`ModelError::Load`] when that runtime is not linked in, refuses a graph, or the graphs do not fit the tracker;
/// [`ModelError::Missing`] when the canonical face can't be read.
pub fn face_tracker(models: &ModelDir, natives: &dyn FileSource) -> Result<FaceTracker, ModelError> {
    let name = runtime_for(models);
    let id = RuntimeId::new(name).map_err(|e| ModelError::Load(e.to_string()))?;
    let runtime =
        find_runtime(&id).ok_or_else(|| ModelError::Load(format!("the `{name}` runtime is not linked in")))?;
    let options = SessionOptions::new(vec![Provider::Cpu], FACE_THREADS);
    let graphs = models.load(&*runtime, natives, &options)?;
    FaceTracker::new(graphs, &models.geometry()?, FaceTrackerConfig::default())
        .map_err(|e| ModelError::Load(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use omuzaki_ai_api::{FetchError, PinnedFile, Progress};

    use super::*;
    use crate::models::MODEL_FILES;

    struct Offline;

    impl FileSource for Offline {
        fn get(&self, _: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
            Err(FetchError::Io { path: "cache".into(), reason: "offline".into() })
        }
    }

    #[test]
    fn the_folder_says_which_runtime_it_needs() {
        let dir = tempfile::tempdir().unwrap();
        // Stand-in TFLite graphs are refused against the pins: a folder is never trusted by its file names alone.
        for name in TFLITE_FILES {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        assert!(matches!(face_models(dir.path()), Err(ModelError::HashMismatch { file }) if file == TFLITE_FILES[0]));

        // An ONNX folder without its manifest is refused too.
        let onnx = tempfile::tempdir().unwrap();
        for name in MODEL_FILES {
            std::fs::write(onnx.path().join(name), name).unwrap();
        }
        assert!(matches!(face_models(onnx.path()), Err(ModelError::Missing(_))));
    }

    #[test]
    fn a_runtime_that_is_not_linked_in_is_named() {
        // This crate links neither LiteRT nor ONNX Runtime; the published bundle's graphs, when supplied, show the
        // error names the runtime.
        let Some(task) = std::env::var_os("OMUZAKI_FACE_TASK") else {
            eprintln!("skipping: set OMUZAKI_FACE_TASK to a downloaded face_landmarker.task");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join(crate::task::TASK_FILE);
        std::fs::copy(task, &copy).unwrap();
        let folder = crate::task::unpack(&copy).unwrap();
        let models = face_models(folder.path()).unwrap();
        assert_eq!(runtime_for(&models), "tflite");
        let Err(ModelError::Load(why)) = face_tracker(&models, &Offline) else { panic!("no runtime is linked in") };
        assert!(why.contains("`tflite` runtime is not linked in"), "{why}");
    }
}
