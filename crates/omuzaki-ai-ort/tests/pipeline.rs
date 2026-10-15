//! Multi-graph models: the fixture stages `a` (y = x + x), `b` (z = p + q) and `cast` (x as int64) run as one pipeline
//! session. Needs `OMUZAKI_ORT_CACHE`; each test skips with a message otherwise.
//!
//! This file holds the only `GlobalAlloc` in its binary: a per-thread counter, as `tests/allocations.rs` has, for the
//! check that a steady-state run allocates nothing outside ONNX Runtime.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a panic is a test failure

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use common::{cache, fixture, local, with_source};
use omuzaki_ai_api::{
    DType, Dim, LoadError, Runtime as _, SessionOptions, Signature, Stage, TensorBuf, TensorRef, TensorSpec,
};
use omuzaki_ai_ort::OrtRuntime;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// The allocator only counts and forwards to `System`, so it adds no obligations beyond `GlobalAlloc`'s own contract;
// implementing that unsafe trait is the one reason this allows `unsafe_code`.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn stage(name: &str, file: &str, inputs: &[(&str, &str)]) -> Stage {
    Stage {
        name: name.to_owned(),
        file: file.to_owned(),
        inputs: inputs.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect(),
    }
}

fn f32_port(name: &str) -> TensorSpec {
    TensorSpec::new(name, DType::F32, [Dim::Fixed(1), Dim::Fixed(4)])
}

fn io(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect()
}

/// Runs `body` with the model's files (the three stage graphs) and `stages`.
fn with_pipeline<R>(
    natives: &common::DirSource,
    stages: &[Stage],
    body: impl FnOnce(&omuzaki_ai_api::ModelSource<'_>) -> R,
) -> R {
    let files = [
        local("a.onnx", &fixture("stage_a.onnx")),
        local("b.onnx", &fixture("stage_b.onnx")),
        local("cast.onnx", &fixture("stage_cast.onnx")),
    ];
    with_source(&files, stages, natives, body)
}

fn a_then_b() -> Vec<Stage> {
    vec![stage("a", "a.onnx", &[("x", "x")]), stage("b", "b.onnx", &[("p", "a.y"), ("q", "x")])]
}

#[test]
fn a_two_stage_pipeline_gives_the_hand_computed_outputs() {
    let Some(natives) = cache("a_two_stage_pipeline_gives_the_hand_computed_outputs") else { return };
    let slot = Signature { inputs: vec![f32_port("x")], outputs: vec![f32_port("z"), f32_port("doubled")] };
    let map = io(&[("z", "b.z"), ("doubled", "a.y")]);
    let mut session =
        with_pipeline(&natives, &a_then_b(), |s| OrtRuntime.load(s, &slot, &map, &SessionOptions::default())).unwrap();
    let mut outputs = TensorBuf::for_outputs(session.signature());
    for x in [[1.0f32, 2.0, 3.0, 4.0], [0.5, -1.0, 10.0, 0.0]] {
        session.run(&[TensorRef::f32(&x, &[1, 4])], &mut outputs).unwrap();
        let doubled: Vec<f32> = x.iter().map(|v| v * 2.0).collect();
        let sum: Vec<f32> = x.iter().map(|v| v * 3.0).collect(); // a.y + x
        assert_eq!(outputs[0].as_f32().unwrap(), sum);
        assert_eq!(outputs[1].as_f32().unwrap(), doubled);
        assert_eq!(outputs[0].shape(), [1, 4]);
    }
    assert_eq!(session.info().provider, omuzaki_ai_api::Provider::Cpu);
    session.reset();
}

#[test]
fn probe_reports_the_slot_inputs_and_the_stage_outputs() {
    let Some(natives) = cache("probe_reports_the_slot_inputs_and_the_stage_outputs") else { return };
    let probe = with_pipeline(&natives, &a_then_b(), |s| OrtRuntime.probe(s)).unwrap();
    assert_eq!(probe.signature.inputs, [f32_port("x")]);
    let outputs: Vec<_> = probe.signature.outputs.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(outputs, ["a.y", "b.z"]);
    assert_eq!(probe.signature.outputs[1], f32_port("b.z"));
    assert!(probe.opset.is_some());
}

fn probe_error(natives: &common::DirSource, stages: &[Stage]) -> String {
    match with_pipeline(natives, stages, |s| OrtRuntime.probe(s)) {
        Err(LoadError::Format(text)) => text,
        other => panic!("expected a format error, got {:?}", other.map(|p| p.signature)),
    }
}

#[test]
fn wiring_to_a_missing_port_is_refused_naming_the_stage() {
    let Some(natives) = cache("wiring_to_a_missing_port_is_refused_naming_the_stage") else { return };
    let stages = vec![stage("a", "a.onnx", &[("x", "x")]), stage("b", "b.onnx", &[("p", "a.nope"), ("q", "x")])];
    let text = probe_error(&natives, &stages);
    assert!(text.contains("stage `b`") && text.contains("input `p`") && text.contains("`a.nope`"), "{text}");
}

#[test]
fn wiring_with_a_type_mismatch_is_refused_naming_the_stage() {
    let Some(natives) = cache("wiring_with_a_type_mismatch_is_refused_naming_the_stage") else { return };
    let stages = vec![
        stage("c", "cast.onnx", &[("x", "x")]),
        stage("b", "b.onnx", &[("p", "c.y"), ("q", "x")]), // int64 into a float input
    ];
    let text = probe_error(&natives, &stages);
    assert!(text.contains("stage `b`") && text.contains("input `p`") && text.contains("`c.y`"), "{text}");
}

#[test]
fn an_io_mapping_to_a_missing_stage_port_fails_accepts() {
    let Some(natives) = cache("an_io_mapping_to_a_missing_stage_port_fails_accepts") else { return };
    let slot = Signature { inputs: vec![f32_port("x")], outputs: vec![f32_port("z")] };
    let map = io(&[("z", "b.nope")]);
    let result = with_pipeline(&natives, &a_then_b(), |s| OrtRuntime.load(s, &slot, &map, &SessionOptions::default()));
    match result {
        Err(LoadError::Format(text)) => {
            assert!(text.contains("does not fit the slot") && text.contains("`z`"), "{text}");
        }
        other => panic!("expected the slot to refuse the model, got {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_missing_stage_file_is_named() {
    let Some(natives) = cache("a_missing_stage_file_is_named") else { return };
    let stages = vec![stage("a", "gone.onnx", &[("x", "x")])];
    let result = with_pipeline(&natives, &stages, |s| OrtRuntime.probe(s));
    assert!(matches!(result, Err(LoadError::MissingFile(name)) if name == "gone.onnx"));
}

#[test]
fn a_pipeline_allocates_nothing_over_1000_steady_state_runs() {
    let Some(natives) = cache("a_pipeline_allocates_nothing_over_1000_steady_state_runs") else { return };
    let slot = Signature { inputs: vec![f32_port("x")], outputs: vec![f32_port("z"), f32_port("doubled")] };
    let map = io(&[("z", "b.z"), ("doubled", "a.y")]);
    let mut session =
        with_pipeline(&natives, &a_then_b(), |s| OrtRuntime.load(s, &slot, &map, &SessionOptions::default())).unwrap();
    let mut outputs = TensorBuf::for_outputs(session.signature());
    let x = [1.0f32, 2.0, 3.0, 4.0];
    let inputs = [TensorRef::f32(&x, &[1, 4])];
    for _ in 0..10 {
        session.run(&inputs, &mut outputs).unwrap(); // warm-up: shapes seen, buffers grown, lazies initialised
    }
    ALLOCATIONS.with(|n| n.set(0));
    WATCHING.with(|w| w.set(true));
    for _ in 0..1000 {
        session.run(&inputs, &mut outputs).unwrap();
    }
    WATCHING.with(|w| w.set(false));
    let total = ALLOCATIONS.with(Cell::get);
    eprintln!("1000 pipeline runs: {total} Rust-side allocations");
    assert_eq!(total, 0);
}
