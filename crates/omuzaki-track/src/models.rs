//! The model folder `tools/models/mediapipe-face.sh` writes: the three ONNX graphs, the canonical face and a
//! `manifest.json` of their SHA-256 hashes. [`ModelDir::open`] checks every file against the manifest, so a graph that
//! was swapped, truncated or corrupted is refused before any runtime sees it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omuzaki_ai_api::{
    FileSource, LocalFile, ModelId, ModelRef, ModelSource, Runtime, Session, SessionOptions, Sha256, Signature,
};
use sha2::{Digest, Sha256 as Hasher};

use crate::{FaceGraphs, GeometryMetadata, blendshapes_signature, detector_signature, landmarks_signature};

/// The folder's files, in load order.
pub const MODEL_FILES: [&str; 4] = [
    "face_detector.onnx",
    "face_landmarks_detector.onnx",
    "face_blendshapes.onnx",
    "geometry_pipeline_metadata_landmarks.binarypb",
];

/// The manifest every folder carries.
const MANIFEST: &str = "manifest.json";

/// Why a model folder could not be opened or loaded.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// A file the folder needs is not there (or cannot be read).
    #[error("missing or unreadable: {}", .0.display())]
    Missing(PathBuf),
    /// A file's SHA-256 is not the one the manifest records.
    #[error("{file} does not match its SHA-256 in the manifest")]
    HashMismatch {
        /// The file's name in the folder.
        file: String,
    },
    /// `manifest.json` is not valid or does not list a file the folder needs.
    #[error("manifest: {0}")]
    Manifest(String),
    /// The metadata did not parse, or the runtime refused a graph.
    #[error("load: {0}")]
    Load(String),
}

/// A verified model folder: every file in [`MODEL_FILES`] is listed in its `manifest.json` and its SHA-256 matches.
#[derive(Debug, Clone)]
pub struct ModelDir {
    path: PathBuf,
    /// The verified SHA-256 and size of each file in [`MODEL_FILES`], in the same order.
    files: [(Sha256, u64); MODEL_FILES.len()],
}

impl ModelDir {
    /// Opens the folder at `dir` and verifies its files against the manifest.
    ///
    /// # Errors
    /// [`ModelError::Missing`] for an absent file, [`ModelError::Manifest`] for a manifest that is malformed or lacks
    /// a file, [`ModelError::HashMismatch`] for a file whose bytes differ from the manifest's hash (never a warning).
    pub fn open(dir: &Path) -> Result<Self, ModelError> {
        let manifest_path = dir.join(MANIFEST);
        let manifest = std::fs::read(&manifest_path).map_err(|_| ModelError::Missing(manifest_path))?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest).map_err(|e| ModelError::Manifest(e.to_string()))?;
        let mut files = [(Sha256([0; 32]), 0); MODEL_FILES.len()];
        for (entry, name) in files.iter_mut().zip(MODEL_FILES) {
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
        Ok(Self { path: dir.to_path_buf(), files })
    }

    /// The canonical face metadata (`geometry_pipeline_metadata_landmarks.binarypb`).
    ///
    /// # Errors
    /// [`ModelError::Missing`] when the file cannot be read, [`ModelError::Load`] when it does not parse.
    pub fn geometry(&self) -> Result<GeometryMetadata, ModelError> {
        let file = self.path.join(MODEL_FILES[3]);
        let bytes = std::fs::read(&file).map_err(|_| ModelError::Missing(file))?;
        GeometryMetadata::parse(&bytes).map_err(|e| ModelError::Load(e.to_string()))
    }

    /// Loads the three graphs with the signatures of [`detector_signature`], [`landmarks_signature`] and
    /// [`blendshapes_signature`] through `runtime`. Cold: the runtime may take seconds.
    ///
    /// The converted graphs keep the names TensorFlow gave their tensors, so each slot port is matched to the model's
    /// own port by position (the slot's order is the graph's), and the runtime then checks names, types and shapes.
    ///
    /// # Errors
    /// [`ModelError::Load`] when the runtime refuses a graph or it does not fit its slot.
    pub fn load(
        &self,
        runtime: &dyn Runtime,
        natives: &dyn FileSource,
        options: &SessionOptions,
    ) -> Result<FaceGraphs, ModelError> {
        let load = |index: usize, slot: &Signature| self.session(index, slot, runtime, natives, options);
        Ok(FaceGraphs {
            detector: load(0, &detector_signature())?,
            landmarks: load(1, &landmarks_signature())?,
            blendshapes: load(2, &blendshapes_signature())?,
        })
    }

    fn session(
        &self,
        index: usize,
        slot: &Signature,
        runtime: &dyn Runtime,
        natives: &dyn FileSource,
        options: &SessionOptions,
    ) -> Result<Box<dyn Session>, ModelError> {
        let name = MODEL_FILES[index];
        // The identity ai-api gives a model: SHA-256 of its file list, `<sha256> <size> <name>\n` per file (here one).
        let (hash, size) = self.files[index];
        let digest = Sha256(Hasher::digest(format!("{hash} {size} {name}\n")).into());
        let stem = name.trim_end_matches(".onnx").replace('_', "-");
        let id = ModelId::new(format!("mediapipe-{stem}")).map_err(|e| ModelError::Load(e.to_string()))?;
        let model =
            ModelRef { id, version: "1.0.0".parse().map_err(|_| ModelError::Load("version".to_owned()))?, digest };
        let files = [LocalFile { name: name.to_owned(), path: self.path.join(name) }];
        let source = ModelSource { model: &model, files: &files, stages: &[], natives };
        let own = runtime.probe(&source).map_err(|e| ModelError::Load(format!("{name}: {e}")))?.signature;
        let io = port_map(slot, &own)
            .ok_or_else(|| ModelError::Load(format!("{name}: its ports do not match the slot's count")))?;
        runtime.load(&source, slot, &io, options).map_err(|e| ModelError::Load(format!("{name}: {e}")))
    }
}

/// The manifest `io` table that renames the model's ports to the slot's. `Signature::accepts` matches ports by name,
/// so a model that already names every slot port (in any order, with extra outputs) needs no renaming; only a model
/// whose names differ (TensorFlow's own) is paired with the slot by position. `None` when positions are the only way
/// to pair them and the counts do not allow it.
fn port_map(slot: &Signature, model: &Signature) -> Option<BTreeMap<String, String>> {
    let mut io = BTreeMap::new();
    for (slot, model, same_count) in [(&slot.inputs, &model.inputs, true), (&slot.outputs, &model.outputs, false)] {
        if slot.iter().all(|s| model.iter().any(|m| m.name == s.name)) {
            continue;
        }
        if if same_count { slot.len() != model.len() } else { slot.len() > model.len() } {
            return None;
        }
        io.extend(slot.iter().zip(model).map(|(s, m)| (s.name.clone(), m.name.clone())));
    }
    Some(io)
}
