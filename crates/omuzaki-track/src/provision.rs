//! The face tracker made ready for the app: the official graphs through the app's verified cache, loaded through a
//! runtime, as the [`FaceTracker`] a [`WebcamTracker`](crate::webcam::WebcamTracker) runs.
//!
//! ```text
//! FileSource::get(task_pin())  ->  task::unpack  ->  ModelDir::load(runtime)  ->  FaceTracker  ->  WebcamTracker
//! ```
//!
//! The app owns the pieces this crate cannot name: the [`FileSource`] over its cache (downloads are verified against
//! the pin before anything reads them), the runtime (the `tflite` runtime, LiteRT, once its library is loaded: the
//! research note `docs/research/2026-10-track-face-budget.md` measured it at twice ONNX Runtime's speed) and the
//! camera. [`face_models`] and [`face_tracker`] are cold: call them off the UI thread.

use omuzaki_ai_api::{FileSource, PinnedFile, Progress, Provider, Runtime, SessionOptions, Sha256};

use crate::models::{ModelDir, ModelError};
use crate::task::{TASK_SHA256, TASK_SIZE, TASK_URL, unpack};
use crate::{FaceTracker, FaceTrackerConfig};

/// Intra-op threads per graph: two cut `track_roi` by a quarter against one, for about half a core at 30 fps
/// (`docs/research/2026-10-track-face-budget.md`).
pub const FACE_THREADS: u16 = 2;

/// The pin of the official `face_landmarker.task`, for the app's [`FileSource`].
pub fn task_pin() -> PinnedFile {
    // The constant is 64 hex digits (the task module's tests open the real bundle against it).
    let sha256 = Sha256::from_hex(TASK_SHA256).unwrap_or(Sha256([0; 32]));
    PinnedFile::new([TASK_URL], sha256, TASK_SIZE)
}

/// The face graphs: the official bundle fetched (or found) through `files`, verified, and unpacked beside it.
///
/// # Errors
/// [`ModelError::Load`] when the bundle can't be fetched, and [`task::unpack`](crate::task::unpack)'s errors.
pub fn face_models(files: &dyn FileSource, progress: &dyn Progress) -> Result<ModelDir, ModelError> {
    let path = files.get(&task_pin(), progress).map_err(|e| ModelError::Load(format!("face_landmarker.task: {e}")))?;
    unpack(&path)
}

/// The tracker over `models`, its graphs loaded through `runtime` on the CPU at [`FACE_THREADS`] threads, in video
/// mode with MediaPipe's thresholds.
///
/// # Errors
/// [`ModelError::Load`] when the runtime refuses a graph or the graphs do not fit the tracker, [`ModelError::Missing`]
/// when the canonical face can't be read.
pub fn face_tracker(
    models: &ModelDir,
    runtime: &dyn Runtime,
    natives: &dyn FileSource,
) -> Result<FaceTracker, ModelError> {
    let options = SessionOptions::new(vec![Provider::Cpu], FACE_THREADS);
    let graphs = models.load(runtime, natives, &options)?;
    FaceTracker::new(graphs, &models.geometry()?, FaceTrackerConfig::default())
        .map_err(|e| ModelError::Load(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use omuzaki_ai_api::{FetchError, NoProgress};

    use super::*;

    /// A cache holding one file for any pin: what the app's verified cache returns after a fetch.
    struct Holding(PathBuf);

    impl FileSource for Holding {
        fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
            assert_eq!(pin.urls, [TASK_URL], "the tracker asks for the official bundle");
            Ok(self.0.clone())
        }
    }

    /// A cache that has nothing and can't download.
    struct Offline;

    impl FileSource for Offline {
        fn get(&self, _: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
            Err(FetchError::Io { path: "cache".into(), reason: "offline".into() })
        }
    }

    #[test]
    fn the_pin_is_the_official_bundle() {
        let pin = task_pin();
        assert_eq!(pin.size, TASK_SIZE);
        assert_eq!(pin.sha256.to_string(), TASK_SHA256);
    }

    #[test]
    fn a_failed_fetch_or_a_wrong_file_is_an_error() {
        assert!(matches!(face_models(&Offline, &NoProgress), Err(ModelError::Load(why)) if why.contains("offline")));
        let dir = tempfile::tempdir().unwrap();
        let wrong = dir.path().join("face_landmarker.task");
        std::fs::write(&wrong, b"not the bundle").unwrap();
        assert!(matches!(face_models(&Holding(wrong), &NoProgress), Err(ModelError::HashMismatch { .. })));
    }
}
