//! The face model folder: three graphs, the canonical face and a `manifest.json` of their SHA-256 hashes. Two kinds:
//!
//! - the ONNX folder `tools/models/mediapipe-face.sh` writes ([`ModelDir::open`], [`MODEL_FILES`]), for the `onnx`
//!   runtime;
//! - the TFLite graphs exactly as MediaPipe publishes them, unpacked from the official `face_landmarker.task`
//!   ([`crate::task`], [`ModelDir::open_tflite`], [`TFLITE_FILES`]), for the `tflite` runtime: no conversion step,
//!   and no manifest: each file is checked against its SHA-256 pinned in [`crate::task::MEMBER_SHA256`].
//!
//! Opening checks every file, so a graph that was swapped, truncated or corrupted is refused before any runtime sees
//! it.

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

/// The files of a folder of the official TFLite graphs, in load order: the members of `face_landmarker.task`.
pub const TFLITE_FILES: [&str; 4] = [
    "face_detector.tflite",
    "face_landmarks_detector.tflite",
    "face_blendshapes.tflite",
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

/// A verified model folder: every file of its kind ([`MODEL_FILES`] or [`TFLITE_FILES`]) is listed in its
/// `manifest.json` and its SHA-256 matches.
#[derive(Debug, Clone)]
pub struct ModelDir {
    path: PathBuf,
    /// The folder's file names, in load order.
    names: &'static [&'static str; 4],
    /// The verified SHA-256 and size of each file in `names`, in the same order.
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
        Self::open_with(dir, &MODEL_FILES, |_, name| {
            manifest["files"][name]["sha256"]
                .as_str()
                .and_then(Sha256::from_hex)
                .ok_or_else(|| ModelError::Manifest(format!("{name} has no valid sha256 entry")))
        })
    }

    /// Opens a folder of the official TFLite graphs ([`TFLITE_FILES`], as [`crate::task::unpack`] writes it) and
    /// verifies each file against its pinned SHA-256, for the `tflite` runtime.
    ///
    /// # Errors
    /// [`ModelError::Missing`] for an absent file, [`ModelError::HashMismatch`] for one that is not the published file.
    pub fn open_tflite(dir: &Path) -> Result<Self, ModelError> {
        Self::open_with(dir, &TFLITE_FILES, |index, _| {
            Sha256::from_hex(crate::task::MEMBER_SHA256[index]).ok_or_else(|| ModelError::Manifest("a pin".to_owned()))
        })
    }

    /// The folder's file names, in load order: [`MODEL_FILES`] or [`TFLITE_FILES`].
    pub fn files(&self) -> &'static [&'static str; 4] {
        self.names
    }

    /// Opens `dir` with `names`, checking each file against the hash `expected(index, name)` gives.
    fn open_with(
        dir: &Path,
        names: &'static [&'static str; 4],
        expected: impl Fn(usize, &str) -> Result<Sha256, ModelError>,
    ) -> Result<Self, ModelError> {
        let mut files = [(Sha256([0; 32]), 0); MODEL_FILES.len()];
        for (index, (entry, &name)) in files.iter_mut().zip(names).enumerate() {
            let listed = expected(index, name)?;
            let file = dir.join(name);
            let bytes = std::fs::read(&file).map_err(|_| ModelError::Missing(file))?;
            let actual = Sha256(Hasher::digest(&bytes).into());
            if actual != listed {
                return Err(ModelError::HashMismatch { file: name.to_owned() });
            }
            *entry = (actual, bytes.len() as u64);
        }
        Ok(Self { path: dir.to_path_buf(), names, files })
    }

    /// The canonical face metadata (`geometry_pipeline_metadata_landmarks.binarypb`).
    ///
    /// # Errors
    /// [`ModelError::Missing`] when the file cannot be read, [`ModelError::Load`] when it does not parse.
    pub fn geometry(&self) -> Result<GeometryMetadata, ModelError> {
        let file = self.path.join(self.names[3]);
        let bytes = std::fs::read(&file).map_err(|_| ModelError::Missing(file))?;
        GeometryMetadata::parse(&bytes).map_err(|e| ModelError::Load(e.to_string()))
    }

    /// Loads the three graphs with the signatures of [`detector_signature`], [`landmarks_signature`] and
    /// [`blendshapes_signature`] through `runtime`. Cold: the runtime may take seconds.
    ///
    /// A graph that already names the slot's ports is matched by name, in any order, as `Signature::accepts` does. The
    /// converted graphs keep the names TensorFlow gave their tensors instead, so their ports are paired with the slot's
    /// by position (the slot's order is the graph's); the runtime then checks names, types and shapes.
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
        let name = self.names[index];
        // The identity ai-api gives a model: SHA-256 of its file list, `<sha256> <size> <name>\n` per file (here one).
        let (hash, size) = self.files[index];
        let digest = Sha256(Hasher::digest(format!("{hash} {size} {name}\n")).into());
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem).replace('_', "-");
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
