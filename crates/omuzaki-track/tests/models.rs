//! The model folder checks of `omuzaki_track::models::ModelDir`, over stand-in files so they need no model and always run.
//! The real graphs, the runtime and the tracker meet in the gated tests that follow in a later change.
#![allow(clippy::unwrap_used)] // a panic is a test failure

use std::sync::Mutex;

use omuzaki_ai_api::{
    FetchError, FileSource, LoadError, ModelSource, PinnedFile, Probe, Progress, Runtime, RuntimeId, Session,
    SessionOptions, Signature,
};
use omuzaki_ai_mock::FakeRuntime;
use omuzaki_track::models::{MODEL_FILES, ModelDir, ModelError};
use omuzaki_track::{blendshapes_signature, detector_signature, landmarks_signature};
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

/// The `fake` runtime's model file for `signature`, with its ports named and ordered by `rename`.
fn fake_model_file(
    signature: &Signature,
    rename: impl Fn(bool, usize, &str) -> String,
    reverse_outputs: bool,
) -> String {
    let mut text = String::from("scale = 1.0\nbias = 0.0\n");
    let mut ports = |input: bool, specs: &[omuzaki_ai_api::TensorSpec]| {
        let mut order: Vec<usize> = (0..specs.len()).collect();
        if !input && reverse_outputs {
            order.reverse();
        }
        for i in order {
            let shape: Vec<String> = specs[i].shape.iter().map(ToString::to_string).collect();
            let dtype = format!("{:?}", specs[i].dtype).to_lowercase();
            let section = if input { "inputs" } else { "outputs" };
            let name = rename(input, i, &specs[i].name);
            let shape =
                shape.iter().map(|d| d.parse::<usize>().map_or(format!("\"{d}\""), |_| d.clone())).collect::<Vec<_>>();
            text += &format!("[[{section}]]\nname = \"{name}\"\ndtype = \"{dtype}\"\nshape = [{}]\n", shape.join(", "));
        }
    };
    ports(true, &signature.inputs);
    ports(false, &signature.outputs);
    text
}

/// A folder whose three graphs are `fake` model files for the real slots, written by `file`.
fn fake_folder(file: impl Fn(&Signature) -> String) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let slots = [detector_signature(), landmarks_signature(), blendshapes_signature()];
    let mut files = serde_json::Map::new();
    for (index, name) in MODEL_FILES.iter().enumerate() {
        let bytes = slots.get(index).map_or_else(|| "stand-in".to_owned(), &file);
        std::fs::write(dir.path().join(name), &bytes).unwrap();
        files.insert((*name).to_owned(), serde_json::json!({ "sha256": format!("{:x}", Sha256::digest(&bytes)) }));
    }
    std::fs::write(dir.path().join("manifest.json"), serde_json::json!({ "files": files }).to_string()).unwrap();
    dir
}

fn load(dir: &tempfile::TempDir) -> Result<omuzaki_track::FaceGraphs, ModelError> {
    ModelDir::open(dir.path()).unwrap().load(&FakeRuntime, &NoFetch, &SessionOptions::default())
}

fn assert_loaded_with_slot_signatures(graphs: &omuzaki_track::FaceGraphs) {
    assert_eq!(graphs.detector.signature(), &detector_signature());
    assert_eq!(graphs.landmarks.signature(), &landmarks_signature());
    assert_eq!(graphs.blendshapes.signature(), &blendshapes_signature());
}

#[test]
fn graphs_that_name_the_slot_ports_load() {
    let dir = fake_folder(|s| fake_model_file(s, |_, _, n| n.to_owned(), false));
    assert_loaded_with_slot_signatures(&load(&dir).unwrap());
}

#[test]
fn outputs_exported_in_another_order_load_by_name() {
    let dir = fake_folder(|s| fake_model_file(s, |_, _, n| n.to_owned(), true));
    assert_loaded_with_slot_signatures(&load(&dir).unwrap());
}

#[test]
fn graphs_with_tensorflows_own_names_are_paired_by_position() {
    let dir = fake_folder(|s| {
        fake_model_file(s, |input, i, _| format!("tf_{}_{i}", if input { "in" } else { "out" }), false)
    });
    assert_loaded_with_slot_signatures(&load(&dir).unwrap());
}

#[test]
fn a_graph_that_does_not_fit_its_slot_is_refused() {
    // A detector with one input too many cannot be paired, and one with the wrong output shape is refused by the runtime.
    let extra_input = fake_folder(|s| {
        let mut with_extra = s.clone();
        with_extra.inputs.push(omuzaki_ai_api::TensorSpec::new(
            "extra",
            omuzaki_ai_api::DType::F32,
            [omuzaki_ai_api::Dim::Fixed(1)],
        ));
        fake_model_file(&with_extra, |input, i, n| if input { format!("tf_in_{i}") } else { n.to_owned() }, false)
    });
    assert!(matches!(load(&extra_input), Err(ModelError::Load(m)) if m.contains("face_detector.onnx")));
    let wrong_shape = fake_folder(|s| {
        let mut bad = s.clone();
        bad.outputs[0].shape = vec![omuzaki_ai_api::Dim::Fixed(7)];
        fake_model_file(&bad, |_, _, n| n.to_owned(), false)
    });
    assert!(matches!(load(&wrong_shape), Err(ModelError::Load(m)) if m.contains("face_detector.onnx")));
}
