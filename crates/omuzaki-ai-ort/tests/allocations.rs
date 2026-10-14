//! A run allocates nothing once the session has seen its shapes (`Session::run`'s contract). Counts the calling thread's
//! allocations over 1000 runs after a warm-up. ONNX Runtime's own C++ allocations don't go through Rust's allocator, so
//! this is the wrapper and `ort`'s Rust code, which is what this crate controls.
//!
//! The counter is per thread, so the two tests here don't see each other. This file holds the only `GlobalAlloc` in its
//! binary. Needs `OMUZAKI_ORT_CACHE`; skips with a message otherwise.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a panic is a test failure

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::{cache, fixture, local, with_source};
use omuzaki_ai_api::{DType, Dim, Runtime as _, SessionOptions, Signature, TensorBuf, TensorRef, TensorSpec};
use omuzaki_ai_ort::OrtRuntime;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// The allocator only counts and forwards to `System`, so it adds no obligations beyond `GlobalAlloc`'s own contract;
// implementing that unsafe trait is the one reason this test file allows `unsafe_code`.
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

/// Warms `session` up on `inputs`, then counts the allocations of 1000 more runs.
fn allocations_in_1000_runs(session: &mut dyn omuzaki_ai_api::Session, inputs: &[TensorRef<'_>]) -> usize {
    let mut outputs = TensorBuf::for_outputs(session.signature());
    for _ in 0..10 {
        session.run(inputs, &mut outputs).unwrap(); // warm-up: shapes seen, buffers grown, lazies initialised
    }
    ALLOCATIONS.with(|n| n.set(0));
    WATCHING.with(|w| w.set(true));
    for _ in 0..1000 {
        session.run(inputs, &mut outputs).unwrap();
    }
    WATCHING.with(|w| w.set(false));
    ALLOCATIONS.with(Cell::get)
}

fn load(file: &str, slot: &Signature, natives: &common::DirSource) -> Box<dyn omuzaki_ai_api::Session> {
    let path = fixture(file);
    let files = [local("model.onnx", &path)];
    with_source(&files, &[], natives, |s| {
        OrtRuntime.load(s, slot, &std::collections::BTreeMap::new(), &SessionOptions::default())
    })
    .unwrap()
}

#[test]
fn a_fixed_shape_model_allocates_nothing_per_run() {
    let Some(natives) = cache("a_fixed_shape_model_allocates_nothing_per_run") else { return };
    let x = TensorSpec::new("x", DType::F32, [Dim::Fixed(1), Dim::Fixed(4)]);
    let slot = Signature { inputs: vec![x.clone()], outputs: vec![TensorSpec { name: "y".into(), ..x }] };
    let mut session = load("identity.onnx", &slot, &natives);
    let input = [1.0f32, 2.0, 3.0, 4.0];
    let total = allocations_in_1000_runs(session.as_mut(), &[TensorRef::f32(&input, &[1, 4])]);
    eprintln!("1000 identity runs: {total} Rust-side allocations");
    assert_eq!(total, 0);
}

#[test]
fn a_dynamic_shape_model_allocates_nothing_once_it_has_seen_the_shape() {
    let Some(natives) = cache("a_dynamic_shape_model_allocates_nothing_once_it_has_seen_the_shape") else { return };
    let any3 = || TensorSpec::new("", DType::F32, [Dim::ANY, Dim::Fixed(3)]);
    let named = |name: &str| TensorSpec { name: name.into(), ..any3() };
    let slot = Signature { inputs: vec![named("a"), named("b")], outputs: vec![named("c")] };
    let mut session = load("add.onnx", &slot, &natives);
    let (a, b) = ([1.0f32; 12], [2.0f32; 12]);
    let inputs = [TensorRef::f32(&a, &[4, 3]), TensorRef::f32(&b, &[4, 3])];
    let total = allocations_in_1000_runs(session.as_mut(), &inputs);
    eprintln!("1000 add runs at [4,3]: {total} Rust-side allocations");
    assert_eq!(total, 0);
}

#[test]
fn a_model_with_more_ports_than_ort_keeps_on_its_stack_still_runs_and_its_allocations_are_measured() {
    let Some(natives) =
        cache("a_model_with_more_ports_than_ort_keeps_on_its_stack_still_runs_and_its_allocations_are_measured")
    else {
        return;
    };
    let port = |name: String| TensorSpec::new(name, DType::F32, [Dim::Fixed(2)]);
    let slot = Signature {
        inputs: (0..7).map(|i| port(format!("x{i}"))).collect(),
        outputs: (0..5).map(|i| port(format!("o{i}"))).collect(),
    };
    let mut session = load("many.onnx", &slot, &natives);
    let data = [[1.0f32, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0], [9.0, 10.0], [11.0, 12.0], [13.0, 14.0]];
    let inputs: Vec<TensorRef<'_>> = data.iter().map(|d| TensorRef::f32(d, &[2])).collect();
    let mut outputs = TensorBuf::for_outputs(session.signature());
    session.run(&inputs, &mut outputs).unwrap();
    assert_eq!(outputs[3].as_f32().unwrap(), [16.0, 18.0]); // x3 + x4
    assert_eq!(outputs[4].as_f32().unwrap(), [24.0, 26.0]); // x5 + x6

    let total = allocations_in_1000_runs(session.as_mut(), &inputs);
    eprintln!("1000 runs, 7 inputs and 5 outputs: {total} Rust-side allocations ({} per run)", total / 1000);
    // Above 6 inputs or 4 outputs, `ort` spills its own lists to the heap on every run, in its code and not ours:
    // `Session::run_with_options` (the input name and value lists), `OutputSelector::resolve_outputs` (output names and
    // values), `Session::run_inner` (the output pointer list) and `SessionOutputs::new`. Removing them needs `unsafe`
    // FFI or a patched `ort`. This pins the measured count (7 per run here) so any change shows.
    assert_eq!(total, 7_000, "ort spills its lists above 6 inputs or 4 outputs; see above");
}

#[test]
fn port_names_of_768_bytes_or_more_cost_ort_its_own_copies_every_run() {
    let Some(natives) = cache("port_names_of_768_bytes_or_more_cost_ort_its_own_copies_every_run") else { return };
    let long = "x".repeat(800);
    let slot = Signature {
        inputs: vec![TensorSpec::new(long, DType::F32, [Dim::Fixed(1), Dim::Fixed(4)])],
        outputs: vec![TensorSpec::new("y", DType::F32, [Dim::Fixed(1), Dim::Fixed(4)])],
    };
    let mut session = load("long_names.onnx", &slot, &natives);
    let input = [1.0f32, 2.0, 3.0, 4.0];
    let total = allocations_in_1000_runs(session.as_mut(), &[TensorRef::f32(&input, &[1, 4])]);
    eprintln!("1000 runs, one 800-byte input name: {total} Rust-side allocations");
    // `ort`'s `with_cstr_ptr_array` (in `Session::run_inner`) copies the name list into fresh `CString`s once the
    // names total 768 bytes or more; every real port name is far shorter. Same exception as above, and pinned.
    assert_eq!(total, 1_000, "one copy of the name list per run");
}
