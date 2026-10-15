//! The `tflite` runtime through `omuzaki-ai-api`: found by its id, `probe` is the scan alone, and `load` refuses
//! until the session arrives (#1750).
#![allow(clippy::doc_markdown, clippy::unwrap_used)] // product names are prose; panics are test failures

mod common;

use std::collections::BTreeMap;

use common::model::{Value, add_graph, bytes, tensor_of};
use common::with_model;
use omuzaki_ai_api::Runtime as _;
use omuzaki_ai_api::{Dim, LoadError, RuntimeId, SessionOptions, Signature, Stage, find_runtime};
use omuzaki_ai_litert::{LiteRtRuntime, RUNTIME_ID};

fn write(dir: &tempfile::TempDir, model: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join("model.tflite");
    std::fs::write(&path, model).unwrap();
    path
}

#[test]
fn the_runtime_is_registered_as_tflite() {
    omuzaki_ai_litert::link();
    assert!(find_runtime(&RuntimeId::new(RUNTIME_ID).unwrap()).is_some());
}

#[test]
fn probe_reads_the_signature_without_the_native_library() {
    let dir = tempfile::tempdir().unwrap();
    let mut model = add_graph();
    tensor_of(&mut model, 0).set(7, Value::Ints(vec![1, -1]));
    let probe = with_model(&write(&dir, &bytes(&model)), &[], |source| LiteRtRuntime.probe(source)).unwrap();
    assert_eq!(probe.opset, None);
    assert_eq!(probe.signature.inputs[0].shape, [Dim::Fixed(1), Dim::ANY]);
    assert_eq!(probe.signature.outputs[0].name, "out");
}

#[test]
fn probe_and_load_refuse_what_the_scan_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let mut model = add_graph();
    tensor_of(&mut model, 2).set(5, Value::Bool(true));
    let path = write(&dir, &bytes(&model));
    let probed = with_model(&path, &[], |source| LiteRtRuntime.probe(source));
    assert!(matches!(probed, Err(LoadError::Refused(_))), "{probed:?}");
    let options = SessionOptions::default();
    let loaded = with_model(&path, &[], |s| LiteRtRuntime.load(s, &Signature::default(), &BTreeMap::new(), &options));
    assert!(matches!(loaded, Err(LoadError::Refused(_))));
}

#[test]
fn load_refuses_until_the_session_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, &bytes(&add_graph()));
    let options = SessionOptions::default();
    let loaded = with_model(&path, &[], |s| LiteRtRuntime.load(s, &Signature::default(), &BTreeMap::new(), &options));
    assert!(matches!(&loaded, Err(LoadError::Runtime(why)) if why.contains("#1750")));
}

#[test]
fn multi_graph_models_and_missing_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, &bytes(&add_graph()));
    let stage = Stage { name: "s".to_owned(), file: "model.tflite".to_owned(), inputs: BTreeMap::new() };
    assert!(matches!(with_model(&path, &[stage], |s| LiteRtRuntime.probe(s)), Err(LoadError::Format(_))));
    let gone = dir.path().join("gone.tflite");
    assert!(matches!(with_model(&gone, &[], |s| LiteRtRuntime.probe(s)), Err(LoadError::MissingFile(_))));
}
