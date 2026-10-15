//! The `onnx` runtime end to end on the tiny fixture models. Tests that need ONNX Runtime read `OMUZAKI_ORT_CACHE` and
//! skip with a message when it is unset. The refusals that happen before any native code runs need no cache.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a panic is a test failure

mod common;

use std::collections::BTreeMap;

use common::{NoNetwork, cache, fixture, local, with_source};
use omuzaki_ai_api::{
    DType, Dim, GraphOptimisation, LoadError, Provider, RunError, Runtime, Session as _, SessionOptions, Signature,
    TensorBuf, TensorRef, TensorSpec, find_runtime,
};
use omuzaki_ai_ort::OrtRuntime;

fn io() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn spec(name: &str, shape: &[Dim]) -> TensorSpec {
    TensorSpec::new(name, DType::F32, shape.to_vec())
}

fn identity_slot() -> Signature {
    Signature {
        inputs: vec![spec("x", &[Dim::Fixed(1), Dim::Fixed(4)])],
        outputs: vec![spec("y", &[Dim::Fixed(1), Dim::Fixed(4)])],
    }
}

#[test]
fn the_runtime_registers_itself_as_onnx() {
    let found = find_runtime(&"onnx".parse().unwrap()).expect("registered");
    assert_eq!(found.id().as_str(), "onnx");
}

#[test]
fn custom_op_domains_and_external_data_are_refused_before_any_library_loads() {
    for (file, word) in [("custom_op.onnx", "com.example.custom"), ("external_data.onnx", "external data")] {
        let path = fixture(file);
        let files = [local("model.onnx", &path)];
        let refused = with_source(&files, &[], &NoNetwork, |source| OrtRuntime.probe(source));
        match refused {
            Err(LoadError::Refused(why)) => assert!(why.contains(word), "{file}: {why}"),
            other => panic!("{file}: {other:?}"),
        }
        let loaded = with_source(&files, &[], &NoNetwork, |s| {
            OrtRuntime.load(s, &identity_slot(), &io(), &SessionOptions::default()).map(|_| ())
        });
        assert!(matches!(loaded, Err(LoadError::Refused(_))), "{file}: {loaded:?}");
    }
}

#[test]
fn multi_graph_models_wait_for_1186() {
    let path = fixture("identity.onnx");
    let files = [local("model.onnx", &path)];
    let stage = omuzaki_ai_api::Stage { name: "a".into(), file: "model.onnx".into(), inputs: BTreeMap::new() };
    let result = with_source(&files, &[stage], &NoNetwork, |source| OrtRuntime.probe(source));
    assert_eq!(result, Err(LoadError::Format("multi-graph models are not supported yet".into())));
}

#[test]
fn a_file_that_is_not_a_model_is_a_format_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("junk.onnx");
    std::fs::write(&path, b"this is not protobuf at all").unwrap();
    let files = [local("junk.onnx", &path)];
    let result = with_source(&files, &[], &NoNetwork, |source| OrtRuntime.probe(source));
    assert!(matches!(result, Err(LoadError::Format(_))), "{result:?}");
    let missing = [local("gone.onnx", &dir.path().join("gone.onnx"))];
    let result = with_source(&missing, &[], &NoNetwork, |source| OrtRuntime.probe(source));
    assert_eq!(result, Err(LoadError::MissingFile("gone.onnx".into())));
}

#[test]
fn probing_the_identity_model_reports_its_signature_and_opset() {
    let Some(natives) = cache("probing_the_identity_model_reports_its_signature_and_opset") else { return };
    let path = fixture("identity.onnx");
    let files = [local("model.onnx", &path)];
    let probe = with_source(&files, &[], &natives, |source| OrtRuntime.probe(source)).unwrap();
    assert_eq!(probe.signature, identity_slot());
    assert_eq!(probe.opset, Some(13));

    // A dynamic dimension is reported as `any`.
    let path = fixture("add.onnx");
    let files = [local("model.onnx", &path)];
    let probe = with_source(&files, &[], &natives, |source| OrtRuntime.probe(source)).unwrap();
    assert_eq!(probe.signature.inputs.len(), 2);
    assert_eq!(probe.signature.outputs, vec![spec("c", &[Dim::ANY, Dim::Fixed(3)])]);
}

#[test]
fn loads_and_runs_the_identity_model_and_reuses_its_output_buffer() {
    let Some(natives) = cache("loads_and_runs_the_identity_model_and_reuses_its_output_buffer") else { return };
    let path = fixture("identity.onnx");
    let files = [local("model.onnx", &path)];
    let mut session =
        with_source(&files, &[], &natives, |s| OrtRuntime.load(s, &identity_slot(), &io(), &SessionOptions::default()))
            .unwrap();
    let info = session.info();
    assert_eq!(info.provider, Provider::Cpu);
    assert!(!info.sandboxed);

    let mut outputs = TensorBuf::for_outputs(session.signature());
    for step in 0..3 {
        let x = [step as f32, 1.0, -2.5, 3.25];
        session.run(&[TensorRef::f32(&x, &[1, 4])], &mut outputs).unwrap();
        assert_eq!(outputs[0].as_f32().unwrap(), x);
        assert_eq!(outputs[0].shape(), [1, 4]);
    }
}

#[test]
fn run_rejects_a_bad_call_the_contract_way() {
    let Some(natives) = cache("run_rejects_a_bad_call_the_contract_way") else { return };
    let path = fixture("identity.onnx");
    let files = [local("model.onnx", &path)];
    let mut session =
        with_source(&files, &[], &natives, |s| OrtRuntime.load(s, &identity_slot(), &io(), &SessionOptions::default()))
            .unwrap();
    let mut outputs = TensorBuf::for_outputs(session.signature());
    let x = [0.0f32; 4];
    assert!(matches!(session.run(&[], &mut outputs), Err(RunError::Mismatch { .. })));
    assert!(matches!(session.run(&[TensorRef::f32(&x, &[4])], &mut outputs), Err(RunError::Mismatch { .. })));
    let mut wrong_type = [TensorBuf::new(DType::I64)];
    assert!(matches!(session.run(&[TensorRef::f32(&x, &[1, 4])], &mut wrong_type), Err(RunError::Mismatch { .. })));
}

#[test]
fn the_slot_names_ports_through_io_and_dynamic_sizes_follow_the_call() {
    let Some(natives) = cache("the_slot_names_ports_through_io_and_dynamic_sizes_follow_the_call") else { return };
    let path = fixture("add.onnx");
    let files = [local("model.onnx", &path)];
    // The slot calls the model's `b`, `a` and `c` by its own names, in another order.
    let slot = Signature {
        inputs: vec![spec("right", &[Dim::ANY, Dim::Fixed(3)]), spec("left", &[Dim::ANY, Dim::Fixed(3)])],
        outputs: vec![spec("sum", &[Dim::ANY, Dim::Fixed(3)])],
    };
    let io: BTreeMap<String, String> = [("left", "a"), ("right", "b"), ("sum", "c")]
        .into_iter()
        .map(|(slot, model)| (slot.to_owned(), model.to_owned()))
        .collect();
    let mut session =
        with_source(&files, &[], &natives, |s| OrtRuntime.load(s, &slot, &io, &SessionOptions::default())).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    for rows in [1usize, 2, 5] {
        let left: Vec<f32> = (0..rows * 3).map(|i| i as f32).collect();
        let right: Vec<f32> = (0..rows * 3).map(|i| 100.0 + i as f32).collect();
        let shape = [rows, 3];
        session.run(&[TensorRef::f32(&right, &shape), TensorRef::f32(&left, &shape)], &mut out).unwrap();
        let expected: Vec<f32> = left.iter().zip(&right).map(|(a, b)| a + b).collect();
        assert_eq!(out[0].as_f32().unwrap(), expected);
        assert_eq!(out[0].shape(), shape);
    }
}

#[test]
fn every_graph_optimisation_level_loads_and_runs_the_same_model() {
    let Some(natives) = cache("every_graph_optimisation_level_loads_and_runs_the_same_model") else { return };
    let path = fixture("add.onnx");
    let files = [local("model.onnx", &path)];
    let slot = Signature {
        inputs: vec![spec("a", &[Dim::ANY, Dim::Fixed(3)]), spec("b", &[Dim::ANY, Dim::Fixed(3)])],
        outputs: vec![spec("c", &[Dim::ANY, Dim::Fixed(3)])],
    };
    for level in [GraphOptimisation::Default, GraphOptimisation::Basic, GraphOptimisation::Extended] {
        let options = SessionOptions::default().with_graph_optimisation(level);
        let mut session = with_source(&files, &[], &natives, |s| OrtRuntime.load(s, &slot, &io(), &options))
            .unwrap_or_else(|e| panic!("{level:?}: {e}"));
        let mut out = TensorBuf::for_outputs(session.signature());
        let (a, b) = ([1.0_f32, 2.0, 3.0], [10.0_f32, 20.0, 30.0]);
        session.run(&[TensorRef::f32(&a, &[1, 3]), TensorRef::f32(&b, &[1, 3])], &mut out).unwrap();
        assert_eq!(out[0].as_f32().unwrap(), [11.0, 22.0, 33.0], "{level:?}");
    }
}

#[test]
fn a_model_that_does_not_fit_the_slot_is_not_loaded() {
    let Some(natives) = cache("a_model_that_does_not_fit_the_slot_is_not_loaded") else { return };
    let path = fixture("identity.onnx");
    let files = [local("model.onnx", &path)];
    let slot =
        Signature { inputs: vec![spec("pcm", &[Dim::Fixed(1), Dim::Fixed(4)])], outputs: vec![spec("y", &[Dim::ANY])] };
    let result = with_source(&files, &[], &natives, |s| {
        OrtRuntime.load(s, &slot, &io(), &SessionOptions::default()).map(|_| ())
    });
    match result {
        Err(LoadError::Format(why)) => assert!(why.contains("pcm") && why.contains('x'), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn asking_for_gpu_providers_a_machine_lacks_falls_back_to_the_cpu_and_says_so() {
    let Some(natives) = cache("asking_for_gpu_providers_a_machine_lacks_falls_back_to_the_cpu_and_says_so") else {
        return;
    };
    let path = fixture("identity.onnx");
    let files = [local("model.onnx", &path)];
    let options = SessionOptions::new(vec![Provider::DirectMl, Provider::Cuda, Provider::WebGpu, Provider::Cpu], 1);
    let session =
        with_source(&files, &[], &natives, |s| OrtRuntime.load_session(s, &identity_slot(), &io(), &options)).unwrap();
    assert_eq!(session.info().provider, Provider::Cpu, "no GPU in the test environment");
    let skipped = session.skipped_providers();
    let providers: Vec<Provider> = skipped.iter().map(|s| s.provider).collect();
    assert_eq!(providers, [Provider::DirectMl, Provider::Cuda, Provider::WebGpu]);
    assert!(skipped.iter().all(|s| !s.reason.is_empty()), "{skipped:?}");
}

#[test]
fn an_output_shape_the_slot_does_not_allow_is_a_mismatch_not_a_result() {
    let Some(natives) = cache("an_output_shape_the_slot_does_not_allow_is_a_mismatch_not_a_result") else { return };
    let path = fixture("add.onnx");
    let files = [local("model.onnx", &path)];
    // The slot promises one row out; the model's dynamic batch follows its inputs, so two rows in give two rows out.
    let slot = Signature {
        inputs: vec![spec("a", &[Dim::ANY, Dim::Fixed(3)]), spec("b", &[Dim::ANY, Dim::Fixed(3)])],
        outputs: vec![spec("c", &[Dim::Fixed(1), Dim::Fixed(3)])],
    };
    let mut session =
        with_source(&files, &[], &natives, |s| OrtRuntime.load(s, &slot, &io(), &SessionOptions::default())).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    let data = [1.0f32; 6];
    let two_rows = [TensorRef::f32(&data, &[2, 3]), TensorRef::f32(&data, &[2, 3])];
    let expected = RunError::Mismatch {
        port: omuzaki_ai_api::Port::Output,
        index: 0,
        problem: omuzaki_ai_api::TensorProblem::Dim { axis: 0, expected: 1, found: 2 },
    };
    // Twice: the first run learns the output sizes, the second uses them; both must refuse.
    for _ in 0..2 {
        assert_eq!(session.run(&two_rows, &mut out), Err(expected.clone()));
    }
    let one_row = [TensorRef::f32(&data[..3], &[1, 3]), TensorRef::f32(&data[..3], &[1, 3])];
    session.run(&one_row, &mut out).unwrap();
    assert_eq!(out[0].shape(), [1, 3]);
}

#[test]
fn a_rejected_call_leaves_every_output_buffer_as_it_was() {
    let Some(natives) = cache("a_rejected_call_leaves_every_output_buffer_as_it_was") else { return };
    let path = fixture("add_sub.onnx");
    let files = [local("model.onnx", &path)];
    // Output `c` takes any number of rows; output `d` is promised one row. Two rows in: `c` fits, `d` does not.
    let slot = Signature {
        inputs: vec![spec("a", &[Dim::ANY, Dim::Fixed(3)]), spec("b", &[Dim::ANY, Dim::Fixed(3)])],
        outputs: vec![spec("c", &[Dim::ANY, Dim::Fixed(3)]), spec("d", &[Dim::Fixed(1), Dim::Fixed(3)])],
    };
    let mut session =
        with_source(&files, &[], &natives, |s| OrtRuntime.load(s, &slot, &io(), &SessionOptions::default())).unwrap();
    let mut out = TensorBuf::for_outputs(session.signature());
    let one = [1.0f32, 2.0, 3.0];
    session.run(&[TensorRef::f32(&one, &[1, 3]), TensorRef::f32(&one, &[1, 3])], &mut out).unwrap();
    let before = out.clone();
    assert_eq!(before[0].as_f32().unwrap(), [2.0, 4.0, 6.0]);

    let two = [5.0f32; 6];
    for _ in 0..2 {
        // Twice: the run that learns the output sizes, and the one that uses them.
        let result = session.run(&[TensorRef::f32(&two, &[2, 3]), TensorRef::f32(&two, &[2, 3])], &mut out);
        assert!(matches!(result, Err(RunError::Mismatch { index: 1, .. })), "{result:?}");
        assert_eq!(out, before, "nothing is written when any output is rejected");
    }
}
