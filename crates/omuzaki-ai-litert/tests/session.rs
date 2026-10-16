//! The session on the real library (#1750): `load` and `run` on the synthetic `ADD` graph, an `any` input resized to
//! the slot's shape, and the shape-change path. Skips with a message without `OMUZAKI_LITERT_CACHE`.
#![allow(clippy::doc_markdown, clippy::unwrap_used)] // product names are prose; panics are test failures

mod common;

use common::model::{Value, add_graph, tensor_of};
use common::{add_slot, library, load_synthetic};
use omuzaki_ai_api::{Dim, Provider, Session as _, TensorBuf, TensorRef};

/// The `ADD` graph with every tensor `[1, -1]` (LiteRT starts it at `[1, 1]`).
fn dynamic_add() -> common::model::Table {
    let mut model = add_graph();
    for index in 0..3 {
        tensor_of(&mut model, index).set(0, Value::Ints(vec![1, 1])).set(7, Value::Ints(vec![1, -1]));
    }
    model
}

#[test]
fn the_fixed_add_graph_loads_and_runs() {
    if !library("the_fixed_add_graph_loads_and_runs") {
        return;
    }
    let mut session = load_synthetic(&add_graph(), &add_slot(&[Dim::Fixed(1), Dim::Fixed(4)]), 1);
    assert_eq!(session.info().provider, Provider::Cpu);
    let mut out = TensorBuf::for_outputs(session.signature());
    let (a, b) = ([1.0f32, 2.0, 3.0, 4.0], [0.5f32; 4]);
    session.run(&[TensorRef::f32(&a, &[1, 4]), TensorRef::f32(&b, &[1, 4])], &mut out).unwrap();
    assert_eq!(out[0].shape(), [1, 4]);
    assert_eq!(out[0].as_f32().unwrap(), [1.5, 2.5, 3.5, 4.5]);
}

#[test]
fn an_any_input_takes_the_slots_shape_and_new_shapes_take_the_cold_path() {
    if !library("an_any_input_takes_the_slots_shape_and_new_shapes_take_the_cold_path") {
        return;
    }
    // The slot fixes [1, 8]: the load resizes the model's [1, -1] to it.
    let mut fixed = load_synthetic(&dynamic_add(), &add_slot(&[Dim::Fixed(1), Dim::Fixed(8)]), 1);
    let mut out = TensorBuf::for_outputs(fixed.signature());
    fixed.run(&[TensorRef::f32(&[1.0; 8], &[1, 8]), TensorRef::f32(&[2.0; 8], &[1, 8])], &mut out).unwrap();
    assert_eq!((out[0].shape(), out[0].as_f32().unwrap()), ([1, 8].as_slice(), [3.0; 8].as_slice()));

    // The slot leaves it open: each new width resizes and reallocates, and the same width again runs as before.
    let mut open = load_synthetic(&dynamic_add(), &add_slot(&[Dim::Fixed(1), Dim::ANY]), 2);
    for width in [3usize, 300, 3, 3] {
        let a: Vec<f32> = (0..width).map(|i| f32::from(u16::try_from(i).unwrap())).collect();
        open.run(&[TensorRef::f32(&a, &[1, width]), TensorRef::f32(&a, &[1, width])], &mut out).unwrap();
        let doubled: Vec<f32> = a.iter().map(|x| x * 2.0).collect();
        assert_eq!((out[0].shape(), out[0].as_f32().unwrap()), ([1, width].as_slice(), doubled.as_slice()));
    }
}
