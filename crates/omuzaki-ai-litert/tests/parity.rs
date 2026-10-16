//! Parity and determinism on the three face graphs (#1751, design §7, ADR 0061 decision 4).
//!
//! - **Determinism:** for one seeded input per graph, the SHA-256 of the outputs is the same over two sessions at each
//!   of 1, 2 and 4 threads, and across the thread counts.
//! - **Parity:** on the seeded and synthetic-face inputs `tools/models/litert_reference.py` wrote, every output element
//!   is within [`WHEEL_TOLERANCE`] of the official wheel's, and within the converter's ONNX gate of `onnxruntime`'s.
//!
//! Gated: `OMUZAKI_LITERT_CACHE` (and `OMUZAKI_LITERT_SHA256`) for the library, `OMUZAKI_TRACK_MODELS` for the face
//! graphs, and for parity `OMUZAKI_LITERT_REFERENCE`, the folder the script wrote. Each test skips with a message
//! without them.
#![allow(clippy::doc_markdown, clippy::unwrap_used)] // product names are prose; panics are test failures

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{face_models, library, load_file, with_model};
use omuzaki_ai_api::{Dim, Runtime as _, Signature, TensorBuf, TensorRef};
use omuzaki_ai_litert::LiteRtRuntime;
use sha2::{Digest, Sha256};

const GRAPHS: [&str; 3] = ["face_detector", "face_landmarks_detector", "face_blendshapes"];

/// The largest |ours - wheel| allowed on any output element (design §7.2).
const WHEEL_TOLERANCE: f32 = 1e-4;

/// The converter's ONNX gate per graph: the largest |tflite - onnx| `mediapipe-face.sh` accepted (design §7.2).
fn onnx_gate(graph: &str) -> f32 {
    match graph {
        "face_detector" => 5.5e-5,
        "face_landmarks_detector" => 3.8e-4,
        _ => 1.8e-6,
    }
}

/// The graph's own signature, from the scan, with every `any` dim fixed to 1: the slot each test loads it under.
fn signature(path: &Path) -> Signature {
    let mut signature = with_model(path, &[], |source| LiteRtRuntime.probe(source)).unwrap().signature;
    for spec in signature.inputs.iter_mut().chain(&mut signature.outputs) {
        spec.shape.iter_mut().filter(|d| **d == Dim::ANY).for_each(|d| *d = Dim::Fixed(1));
    }
    signature
}

fn fixed_shape(spec: &omuzaki_ai_api::TensorSpec) -> Vec<usize> {
    spec.shape.iter().map(|d| if let Dim::Fixed(n) = d { *n } else { 1 }).collect()
}

/// A seeded input in [0, 1) from a fixed linear congruential sequence, so no dependency is needed for it.
fn seeded(len: usize) -> Vec<f32> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            f32::from(u16::try_from(state >> 48).unwrap()) / 65_536.0
        })
        .collect()
}

/// Runs `session` once on `inputs` and returns its outputs.
fn run(session: &mut dyn omuzaki_ai_api::Session, inputs: &[(Vec<f32>, Vec<usize>)]) -> Vec<TensorBuf> {
    let refs: Vec<TensorRef<'_>> = inputs.iter().map(|(data, shape)| TensorRef::f32(data, shape)).collect();
    let mut outputs = TensorBuf::for_outputs(session.signature());
    session.run(&refs, &mut outputs).unwrap();
    outputs
}

fn digest(outputs: &[TensorBuf]) -> String {
    let mut hasher = Sha256::new();
    for output in outputs {
        for value in output.as_f32().unwrap() {
            hasher.update(value.to_le_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

#[test]
fn outputs_are_bit_identical_over_sessions_and_thread_counts() {
    let test = "outputs_are_bit_identical_over_sessions_and_thread_counts";
    let Some(models) = face_models(test) else { return };
    if !library(test) {
        return;
    }
    for graph in GRAPHS {
        let path = models.join(format!("{graph}.tflite"));
        let slot = signature(&path);
        let inputs: Vec<(Vec<f32>, Vec<usize>)> = slot
            .inputs
            .iter()
            .map(|spec| {
                let shape = fixed_shape(spec);
                (seeded(shape.iter().product()), shape)
            })
            .collect();
        let mut digests = Vec::new();
        for threads in [1u16, 2, 4] {
            for _ in 0..2 {
                let mut session = load_file(&path, &slot, threads);
                digests.push((threads, digest(&run(&mut session, &inputs))));
            }
        }
        eprintln!("{graph}: {digests:?}");
        assert!(digests.iter().all(|(_, d)| *d == digests[0].1), "{graph}: {digests:?}");
    }
}

/// One tensor of the reference index.
struct Entry {
    shape: Vec<usize>,
    file: PathBuf,
}

impl Entry {
    fn read(&self) -> Vec<f32> {
        let bytes = std::fs::read(&self.file).unwrap();
        bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
    }
}

/// `reference.txt`: (graph, case) to role to the tensors in index order.
type Index = BTreeMap<(String, String), BTreeMap<String, Vec<Entry>>>;

fn read_index(dir: &Path) -> Index {
    let mut index = Index::new();
    for line in std::fs::read_to_string(dir.join("reference.txt")).unwrap().lines() {
        let [graph, case, role, position, dtype, shape, file] = line.split(' ').collect::<Vec<_>>()[..] else {
            panic!("a malformed line in reference.txt: {line:?}");
        };
        assert_eq!(dtype, "f32", "{line}");
        let shape = if shape == "-" { Vec::new() } else { shape.split('x').map(|d| d.parse().unwrap()).collect() };
        let list = index.entry((graph.to_owned(), case.to_owned())).or_default().entry(role.to_owned()).or_default();
        assert_eq!(position.parse::<usize>().unwrap(), list.len(), "{line}: out of order");
        list.push(Entry { shape, file: dir.join(file) });
    }
    index
}

/// The largest |a - b| over two equally long lists; a non-finite value on either side fails.
fn max_diff(ours: &[f32], theirs: &[f32], what: &str) -> f32 {
    assert_eq!(ours.len(), theirs.len(), "{what}: lengths differ");
    ours.iter().zip(theirs).fold(0.0f32, |worst, (a, b)| {
        assert!(a.is_finite() && b.is_finite(), "{what}: a non-finite value");
        worst.max((a - b).abs())
    })
}

#[test]
fn outputs_match_the_wheel_and_stay_inside_the_onnx_gate() {
    let test = "outputs_match_the_wheel_and_stay_inside_the_onnx_gate";
    let Some(models) = face_models(test) else { return };
    let Some(reference) = std::env::var_os("OMUZAKI_LITERT_REFERENCE").map(PathBuf::from) else {
        eprintln!("skipping {test}: set OMUZAKI_LITERT_REFERENCE to the folder tools/models/litert_reference.py wrote");
        return;
    };
    if !library(test) {
        return;
    }
    let index = read_index(&reference);
    let mut rows = Vec::new();
    for graph in GRAPHS {
        let path = models.join(format!("{graph}.tflite"));
        let slot = signature(&path);
        let mut session = load_file(&path, &slot, 1);
        let cases: Vec<_> = index.iter().filter(|((g, _), _)| g == graph).collect();
        assert!(cases.iter().any(|((_, c), _)| c.starts_with("seeded")), "{graph}: no seeded case");
        if graph != "face_blendshapes" {
            assert!(cases.iter().any(|((_, c), _)| c.starts_with("face")), "{graph}: no synthetic face");
        }
        for ((_, case), roles) in cases {
            let inputs: Vec<_> = roles["in"].iter().map(|e| (e.read(), e.shape.clone())).collect();
            let ours = run(&mut session, &inputs);
            let (wheel, onnx) = (&roles["wheel"], &roles["onnx"]);
            assert_eq!((wheel.len(), onnx.len()), (ours.len(), ours.len()), "{graph} {case}: output counts");
            let (mut worst_wheel, mut worst_onnx) = (0.0f32, 0.0f32);
            for (i, out) in ours.iter().enumerate() {
                let what = format!("{graph} {case} output {i}");
                assert_eq!(out.shape(), wheel[i].shape, "{what}: shape");
                worst_wheel = worst_wheel.max(max_diff(out.as_f32().unwrap(), &wheel[i].read(), &what));
                worst_onnx = worst_onnx.max(max_diff(out.as_f32().unwrap(), &onnx[i].read(), &what));
            }
            rows.push(format!(
                "| {graph} | {case} | {worst_wheel:.3e} | {worst_onnx:.3e} (gate {:.1e}) |",
                onnx_gate(graph)
            ));
            assert!(worst_wheel <= WHEEL_TOLERANCE, "{graph} {case}: |ours - wheel| = {worst_wheel:e}");
            assert!(worst_onnx <= onnx_gate(graph), "{graph} {case}: |ours - onnx| = {worst_onnx:e}");
        }
    }
    eprintln!(
        "| graph | input | max abs diff vs wheel | max abs diff vs onnx |\n|---|---|---|---|\n{}",
        rows.join("\n")
    );
}
