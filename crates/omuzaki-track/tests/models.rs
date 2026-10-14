//! The model folder checks of `omuzaki_track::models::ModelDir`, over stand-in files so they need no model and always run.
//! The real graphs, the runtime and the tracker meet in the gated tests that follow in a later change.
#![allow(clippy::unwrap_used)] // a panic is a test failure

use std::sync::Mutex;

use omuzaki_ai_api::{
    FetchError, FileSource, LoadError, ModelSource, PinnedFile, Probe, Progress, Runtime, RuntimeId, Session,
    SessionOptions, Signature,
};
use omuzaki_track::models::{MODEL_FILES, ModelDir, ModelError};
use sha2::{Digest, Sha256};

/// A folder of stand-in files with a matching manifest, for the checks that need no model.
fn stand_in_folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut files = serde_json::Map::new();
    for name in MODEL_FILES {
        let bytes = format!("stand-in for {name}");
        std::fs::write(dir.path().join(name), &bytes).unwrap();
        files.insert(name.to_owned(), serde_json::json!({ "sha256": format!("{:x}", Sha256::digest(&bytes)) }));
    }
    std::fs::write(dir.path().join("manifest.json"), serde_json::json!({ "files": files }).to_string()).unwrap();
    dir
}

#[test]
fn a_folder_matching_its_manifest_opens() {
    assert!(ModelDir::open(stand_in_folder().path()).is_ok());
}

#[test]
fn a_missing_file_or_manifest_is_refused() {
    let dir = stand_in_folder();
    std::fs::remove_file(dir.path().join(MODEL_FILES[2])).unwrap();
    assert!(matches!(ModelDir::open(dir.path()), Err(ModelError::Missing(p)) if p.ends_with(MODEL_FILES[2])));
    std::fs::remove_file(dir.path().join("manifest.json")).unwrap();
    assert!(matches!(ModelDir::open(dir.path()), Err(ModelError::Missing(_))));
}

#[test]
fn a_manifest_without_a_file_is_refused() {
    let dir = stand_in_folder();
    std::fs::write(dir.path().join("manifest.json"), r#"{"files":{}}"#).unwrap();
    assert!(matches!(ModelDir::open(dir.path()), Err(ModelError::Manifest(_))));
    std::fs::write(dir.path().join("manifest.json"), "not json").unwrap();
    assert!(matches!(ModelDir::open(dir.path()), Err(ModelError::Manifest(_))));
}

#[test]
fn a_changed_byte_is_a_hash_mismatch() {
    let dir = stand_in_folder();
    std::fs::write(dir.path().join(MODEL_FILES[0]), "tampered").unwrap();
    assert!(matches!(ModelDir::open(dir.path()), Err(ModelError::HashMismatch { file }) if file == MODEL_FILES[0]));
}

/// A runtime that records the identity of each model it is asked to probe, then refuses it.
struct Recording(Mutex<Vec<(String, String)>>);

impl Runtime for Recording {
    fn id(&self) -> RuntimeId {
        RuntimeId::new("recording").unwrap()
    }

    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        self.0.lock().unwrap().push((source.files[0].name.clone(), source.model.digest.to_string()));
        Err(LoadError::MissingFile("recorded".to_owned()))
    }

    fn load(
        &self,
        _: &ModelSource<'_>,
        _: &Signature,
        _: &std::collections::BTreeMap<String, String>,
        _: &SessionOptions,
    ) -> Result<Box<dyn Session>, LoadError> {
        Err(LoadError::MissingFile("recorded".to_owned()))
    }
}

struct NoFetch;

impl FileSource for NoFetch {
    fn get(&self, _: &PinnedFile, _: &dyn Progress) -> Result<std::path::PathBuf, FetchError> {
        panic!("the runtime is not asked for a library");
    }
}

#[test]
fn a_graphs_identity_is_the_digest_of_its_file_list() {
    let dir = stand_in_folder();
    let runtime = Recording(Mutex::new(Vec::new()));
    let result = ModelDir::open(dir.path()).unwrap().load(&runtime, &NoFetch, &SessionOptions::default());
    assert!(matches!(result, Err(ModelError::Load(_))));
    // ai-api's `digest_text` for one file: `<sha256> <size> <name>\n`, hashed again with SHA-256.
    let name = MODEL_FILES[0];
    let bytes = std::fs::read(dir.path().join(name)).unwrap();
    let text = format!("{:x} {} {name}\n", Sha256::digest(&bytes), bytes.len());
    assert_eq!(*runtime.0.lock().unwrap(), [(name.to_owned(), format!("{:x}", Sha256::digest(text)))]);
}
