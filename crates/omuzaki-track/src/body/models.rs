//! The body model folder `tools/models/mediapipe-body.sh` writes: the palm detector and hand landmark model of
//! `hand_landmarker.task`, the pose detector, and the pose landmark model in its lite and full sizes, as ONNX graphs
//! with a `manifest.json` of their SHA-256 hashes. Opening checks every file the chosen size needs, so a graph that was
//! swapped, truncated or corrupted is refused before any runtime sees it.

use std::path::{Path, PathBuf};

use omuzaki_ai_api::{FileSource, Provider, Runtime, RuntimeId, SessionOptions, Sha256, find_runtime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256 as Hasher};

use super::{
    BodyGraphs, BodyTracker, BodyTrackerConfig, hand_signature, palm_signature, pose_detector_signature,
    pose_landmarks_signature,
};
use crate::models::{ModelError, load_graph};

/// The pose landmark model's size: lite tracks at about half the cost, full is steadier on hands far from the camera
/// and on unusual poses (the "precise" switch).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoseSize {
    /// `pose_landmarks_lite.onnx`.
    #[default]
    Lite,
    /// `pose_landmarks_full.onnx`.
    Full,
}

/// The folder's files: the palm detector, the hand landmarks, the pose detector, then the pose landmarks by size.
pub const BODY_FILES: [&str; 5] = [
    "hand_detector.onnx",
    "hand_landmarks_detector.onnx",
    "pose_detector.onnx",
    "pose_landmarks_lite.onnx",
    "pose_landmarks_full.onnx",
];

const PALM: usize = 0;
const HAND: usize = 1;
const POSE_DETECTOR: usize = 2;

/// Intra-op threads per graph: the face tracker's two (`crate::provision::FACE_THREADS`), so a body pass costs about
/// what a face pass does per graph.
pub const BODY_THREADS: u16 = 2;

/// A verified body model folder, for one pose size.
#[derive(Debug, Clone)]
pub struct BodyModelDir {
    path: PathBuf,
    size: PoseSize,
    /// The verified SHA-256 and size of the palm, hand, pose detector and chosen pose landmark files.
    files: [(Sha256, u64); 4],
}

impl BodyModelDir {
    /// Opens the folder at `dir` for the pose `size` and verifies the four files it needs against the manifest.
    ///
    /// # Errors
    /// [`ModelError::Missing`] for an absent file, [`ModelError::Manifest`] for a manifest that is malformed or lacks
    /// a file, [`ModelError::HashMismatch`] for a file whose bytes differ from the manifest's hash.
    pub fn open(dir: &Path, size: PoseSize) -> Result<Self, ModelError> {
        let manifest_path = dir.join("manifest.json");
        let manifest = std::fs::read(&manifest_path).map_err(|_| ModelError::Missing(manifest_path))?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest).map_err(|e| ModelError::Manifest(e.to_string()))?;
        let mut files = [(Sha256([0; 32]), 0); 4];
        for (entry, name) in files.iter_mut().zip(Self::names(size)) {
            let listed = manifest["files"][name]["sha256"]
                .as_str()
                .and_then(Sha256::from_hex)
                .ok_or_else(|| ModelError::Manifest(format!("{name} has no valid sha256 entry")))?;
            let file = dir.join(name);
            let bytes = std::fs::read(&file).map_err(|_| ModelError::Missing(file))?;
            let actual = Sha256(Hasher::digest(&bytes).into());
            if actual != listed {
                return Err(ModelError::HashMismatch { file: name.to_owned() });
            }
            *entry = (actual, bytes.len() as u64);
        }
        Ok(Self { path: dir.to_path_buf(), size, files })
    }

    /// The four files a pose size needs, in load order.
    fn names(size: PoseSize) -> [&'static str; 4] {
        let pose = match size {
            PoseSize::Lite => BODY_FILES[3],
            PoseSize::Full => BODY_FILES[4],
        };
        [BODY_FILES[PALM], BODY_FILES[HAND], BODY_FILES[POSE_DETECTOR], pose]
    }

    /// The folder.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The pose size it was opened for.
    pub fn size(&self) -> PoseSize {
        self.size
    }

    /// The four graphs loaded through `runtime`, each into its slot. Cold: the runtime may take seconds.
    ///
    /// # Errors
    /// [`ModelError::Load`] when the runtime refuses a graph or a graph does not fit its slot.
    pub fn load(
        &self,
        runtime: &dyn Runtime,
        natives: &dyn FileSource,
        options: &SessionOptions,
    ) -> Result<BodyGraphs, ModelError> {
        let names = Self::names(self.size);
        let load = |i: usize, slot| load_graph(&self.path, names[i], self.files[i], &slot, runtime, natives, options);
        Ok(BodyGraphs {
            palm: load(0, palm_signature())?,
            hands: [load(1, hand_signature())?, load(1, hand_signature())?],
            pose_detector: load(2, pose_detector_signature())?,
            pose_landmarks: load(3, pose_landmarks_signature())?,
        })
    }

    /// The tracker over these graphs, loaded through the `onnx` runtime (found among the runtimes linked into the
    /// app) on the CPU at [`BODY_THREADS`] threads. Cold: the runtime may take seconds.
    ///
    /// # Errors
    /// [`ModelError::Load`] when the runtime is not linked in, refuses a graph, or a graph does not fit its slot.
    pub fn tracker(&self, natives: &dyn FileSource, config: BodyTrackerConfig) -> Result<BodyTracker, ModelError> {
        let id = RuntimeId::new("onnx").map_err(|e| ModelError::Load(e.to_string()))?;
        let runtime = find_runtime(&id).ok_or_else(|| ModelError::Load("the `onnx` runtime is not linked in".into()))?;
        let options = SessionOptions::new(vec![Provider::Cpu], BODY_THREADS);
        BodyTracker::new(self.load(&*runtime, natives, &options)?, config).map_err(|e| ModelError::Load(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_checked_against_its_manifest() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(BodyModelDir::open(dir.path(), PoseSize::Lite), Err(ModelError::Missing(_))));
        for name in BODY_FILES {
            std::fs::write(dir.path().join(name), name).unwrap_or_else(|e| panic!("{e}"));
        }
        let hash = |name: &str| hex(&Hasher::digest(name.as_bytes()));
        let mut files = serde_json::Map::new();
        for name in BODY_FILES {
            files.insert(name.to_owned(), serde_json::json!({ "sha256": hash(name) }));
        }
        // The full model's entry is wrong: the lite folder still opens, the full one is refused.
        files.insert(BODY_FILES[4].to_owned(), serde_json::json!({ "sha256": hash("something else") }));
        let manifest = serde_json::json!({ "files": files });
        std::fs::write(dir.path().join("manifest.json"), manifest.to_string()).unwrap_or_else(|e| panic!("{e}"));
        let lite = BodyModelDir::open(dir.path(), PoseSize::Lite).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(lite.size(), PoseSize::Lite);
        assert!(matches!(
            BodyModelDir::open(dir.path(), PoseSize::Full),
            Err(ModelError::HashMismatch { file }) if file == BODY_FILES[4]
        ));
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
