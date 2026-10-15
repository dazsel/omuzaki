//! The model scan against design §5's allow-list: the fixed-shape `ADD` graph is accepted, and one case per row of the
//! table is refused with the right `LoadError`, on synthetic FlatBuffers built here (`common/model.rs`).
#![allow(clippy::doc_markdown)] // product names are prose

mod common;

use common::model::{Table, Value, add_graph, builtin, bytes, operator_of, slot, tensor, tensor_of};
use omuzaki_ai_api::{DType, Dim, LoadError, Signature, TensorSpec};
use omuzaki_ai_litert::scan::{EXCLUDED, builtin_name, scan_model};

fn scan(model: &Table) -> Result<Signature, LoadError> {
    scan_model(&bytes(model)).map(|scan| scan.signature)
}

/// `model` changed by `change`, then scanned.
fn scan_with(change: impl FnOnce(&mut Table)) -> Result<Signature, LoadError> {
    let mut model = add_graph();
    change(&mut model);
    scan(&model)
}

/// Asserts a `Refused` whose text contains `needle`.
#[track_caller]
fn refused(result: Result<Signature, LoadError>, needle: &str) {
    match result {
        Err(LoadError::Refused(why)) => assert!(why.contains(needle), "{needle:?} not in {why:?}"),
        other => panic!("expected a refusal with {needle:?}, got {other:?}"),
    }
}

/// Asserts a `Format` error.
#[track_caller]
fn malformed(result: Result<Signature, LoadError>) {
    match result {
        Err(LoadError::Format(_)) => {}
        other => panic!("expected a malformed file, got {other:?}"),
    }
}

fn f32(name: &str, shape: &[Dim]) -> TensorSpec {
    TensorSpec::new(name, DType::F32, shape.to_vec())
}

#[test]
fn the_fixed_shape_add_graph_is_accepted_with_its_signature() {
    let four = [Dim::Fixed(1), Dim::Fixed(4)];
    let signature = scan(&add_graph()).unwrap();
    assert_eq!(signature.inputs, [f32("a", &four), f32("b", &four)]);
    assert_eq!(signature.outputs, [f32("out", &four)]);
}

#[test]
fn a_dynamic_input_probes_as_any() {
    let signature = scan_with(|m| {
        tensor_of(m, 0).set(0, Value::Ints(vec![1, 1, 1, 3])).set(7, Value::Ints(vec![1, -1, -1, 3]));
    })
    .unwrap();
    assert_eq!(signature.inputs[0].shape, [Dim::Fixed(1), Dim::ANY, Dim::ANY, Dim::Fixed(3)]);
    // A signature that contradicts its shape is malformed.
    malformed(scan_with(|m| {
        tensor_of(m, 0).set(7, Value::Ints(vec![2, -1]));
    }));
}

#[test]
fn custom_and_off_list_operators_are_refused() {
    refused(
        scan_with(|m| {
            m.at(slot::CODES, 0).set(1, Value::Str("FlexAddV2")).set(3, Value::I32(32));
        }),
        "custom operator `FlexAddV2`",
    );
    // CONCAT_EMBEDDINGS is in the schema, but LiteRT 2.3.0 registers no kernel for it.
    refused(scan_with(|m| *m.at(slot::CODES, 0) = builtin(29)), "`CONCAT_EMBEDDINGS` (29)");
    refused(scan_with(|m| *m.at(slot::CODES, 0) = builtin(32)), "`CUSTOM` (32)");
    refused(scan_with(|m| *m.at(slot::CODES, 0) = builtin(500)), "`unknown` (500)");
}

#[test]
fn every_excluded_builtin_is_refused_by_name() {
    for &(code, why) in EXCLUDED {
        refused(scan_with(|m| *m.at(slot::CODES, 0) = builtin(code)), &format!("{}: {why}", builtin_name(code)));
    }
}

#[test]
fn the_allow_list_is_the_builds_registered_builtins_less_the_exclusions() {
    let accepted: Vec<i32> =
        (0..=209).filter(|&code| scan_with(|m| *m.at(slot::CODES, 0) = builtin(code)).is_ok()).collect();
    // `tflite/core/kernels/register.cc` at v2.3.0 registers 172 builtins; every exclusion is one of them.
    assert_eq!(accepted.len(), 172 - EXCLUDED.len());
    // The names come from the pinned schema's enum, in code order.
    assert_eq!(builtin_name(146), "`RANDOM_STANDARD_NORMAL` (146)");
    assert_eq!(builtin_name(209), "`STABLEHLO_CASE` (209)");
    assert_eq!(builtin_name(210), "`unknown` (210)");
    // The 22 builtins the three face graphs use (design §5).
    for face in [0, 2, 3, 4, 6, 42, 14, 17, 40, 18, 59, 34, 54, 19, 22, 76, 75, 99, 45, 41, 74, 39] {
        assert!(accepted.contains(&face), "{} must stay allowed", builtin_name(face));
    }
    // The schema rule: a code above 127 is read from `builtin_code`, whatever the deprecated byte says.
    assert!(scan_with(|m| *m.at(slot::CODES, 0) = Table(vec![(0, Value::I8(127)), (3, Value::I32(150))])).is_ok());
}

#[test]
fn a_second_subgraph_is_refused() {
    refused(
        scan_with(|m| {
            let first = m.at(slot::SUBGRAPHS, 0).clone();
            m.set(slot::SUBGRAPHS, Value::Tables(vec![first.clone(), first]));
        }),
        "2 subgraphs",
    );
}

#[test]
fn state_is_refused_variables_and_mutating_inputs() {
    refused(scan_with(|m| _ = tensor_of(m, 2).set(5, Value::Bool(true))), "tensor `out` is a variable");
    refused(scan_with(|m| _ = operator_of(m).set(7, Value::Bools(vec![false, true]))), "mutates a variable input");
    // All-false is the schema's "nothing mutated".
    assert!(scan_with(|m| _ = operator_of(m).set(7, Value::Bools(vec![false, false]))).is_ok());
}

#[test]
fn operator_extras_are_refused() {
    refused(scan_with(|m| _ = operator_of(m).set(8, Value::Ints(vec![2]))), "intermediate tensors");
    refused(scan_with(|m| _ = operator_of(m).set(5, Value::Bytes(vec![1]))), "custom options");
    refused(scan_with(|m| _ = operator_of(m).set(9, Value::U64(64))), "custom options");
    refused(scan_with(|m| _ = operator_of(m).set(11, Value::I8(1))), "builtin_options_2");
    // An omitted optional input is -1; an omitted output is malformed, as is a tensor index out of range.
    assert!(scan_with(|m| _ = operator_of(m).set(1, Value::Ints(vec![0, 1, -1]))).is_ok());
    malformed(scan_with(|m| _ = operator_of(m).set(2, Value::Ints(vec![-1]))));
    malformed(scan_with(|m| _ = operator_of(m).set(1, Value::Ints(vec![0, 3]))));
    malformed(scan_with(|m| _ = operator_of(m).set(0, Value::U32(1))));
}

#[test]
fn port_names_must_be_unique_and_non_empty() {
    refused(scan_with(|m| _ = tensor_of(m, 1).set(3, Value::Str("a"))), "two graph inputs are named `a`");
    refused(scan_with(|m| _ = tensor_of(m, 0).set(3, Value::Str(""))), "a graph port named ``");
    // An input and an output may share a name: they are different lists.
    assert!(scan_with(|m| _ = tensor_of(m, 2).set(3, Value::Str("a"))).is_ok());
}

#[test]
fn a_port_of_unknown_rank_is_refused_and_a_scalar_is_not() {
    refused(scan_with(|m| _ = tensor_of(m, 0).set(0, Value::Ints(vec![]))), "graph port `a` has an unknown rank");
    let scalar = scan_with(|m| _ = tensor_of(m, 0).set(0, Value::Ints(vec![])).set(8, Value::Bool(true))).unwrap();
    assert_eq!(scalar.inputs[0].shape, []);
    // Internal tensors may have an unknown rank: the interpreter computes it.
    let tensors = vec![tensor("a", &[1, 4]), tensor("b", &[1, 4]), tensor("out", &[1, 4]), tensor("t", &[])];
    assert!(scan_with(|m| _ = m.at(slot::SUBGRAPHS, 0).set(slot::TENSORS, Value::Tables(tensors))).is_ok());
}

#[test]
fn ports_carry_dtypes_and_no_data() {
    refused(
        scan_with(|m| {
            m.set(slot::BUFFERS, Value::Tables(vec![Table::default(), Table(vec![(0, Value::Bytes(vec![0; 16]))])]));
            tensor_of(m, 1).set(2, Value::U32(1));
        }),
        "graph input 1 is a constant",
    );
    refused(
        scan_with(|m| _ = tensor_of(m, 0).set(4, Value::Table(Table::default()))),
        "graph input 0 is a constant or quantized",
    );
    malformed(scan_with(|m| _ = tensor_of(m, 0).set(1, Value::I8(1)))); // FLOAT16 is internal only
    refused(scan_with(|m| _ = tensor_of(m, 0).set(1, Value::I8(5))), "element type 5"); // STRING
}

#[test]
fn data_outside_the_model_is_refused() {
    refused(scan_with(|m| _ = tensor_of(m, 2).set(10, Value::U32(1))), "external buffer");
    refused(scan_with(|m| _ = m.set(9, Value::Tables(vec![Table::default()]))), "external buffers");
    refused(
        scan_with(|m| {
            _ = m.set(slot::BUFFERS, Value::Tables(vec![Table(vec![(1, Value::U64(64)), (2, Value::U64(8))])]));
        }),
        "outside the model",
    );
}

#[test]
fn sparsity_quantization_details_and_variants_are_refused() {
    refused(scan_with(|m| _ = tensor_of(m, 2).set(6, Value::Table(Table::default()))), "is sparse");
    refused(scan_with(|m| _ = tensor_of(m, 2).set(9, Value::Tables(vec![Table::default()]))), "variant subtypes");
    let details = Table(vec![(4, Value::I8(1)), (5, Value::Table(Table::default()))]);
    refused(scan_with(|m| _ = tensor_of(m, 2).set(4, Value::Table(details))), "quantization other than affine");
}

#[test]
fn only_descriptive_metadata_is_accepted() {
    let metadata = |name| Value::Tables(vec![Table(vec![(0, Value::Str(name)), (1, Value::U32(0))])]);
    assert!(scan_with(|m| _ = m.set(6, metadata("min_runtime_version"))).is_ok());
    refused(scan_with(|m| _ = m.set(6, metadata("reduced_precision_support"))), "metadata `reduced_precision_support`");
}

#[test]
fn a_field_newer_than_the_pinned_schema_is_refused() {
    refused(scan_with(|m| _ = tensor_of(m, 0).set(11, Value::U32(1))), "a Tensor field this runtime does not know");
    refused(scan_with(|m| _ = m.set(10, Value::U32(1))), "a Model field this runtime does not know");
    refused(scan_with(|m| _ = operator_of(m).set(14, Value::I32(1))), "a Operator field");
}

#[test]
fn truncated_out_of_range_and_corrupted_files_are_errors_never_panics() {
    let good = bytes(&add_graph());
    for len in 0..good.len() {
        assert!(scan_model(&good[..len]).is_err(), "a prefix of {len} bytes");
    }
    // The root offset past the end, then every single byte flipped: an error or a valid scan, never a panic.
    let mut far = good.clone();
    far[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(scan_model(&far), Err(LoadError::Format(_))));
    for i in 0..good.len() {
        let mut flipped = good.clone();
        flipped[i] ^= 0xff;
        let _ = scan_model(&flipped);
    }
}

#[test]
fn only_schema_version_3_is_read() {
    refused(scan_with(|m| _ = m.set(0, Value::U32(2))), "schema version 2");
}
