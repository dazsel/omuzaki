//! `PipelineSession::run` on the fixture stages (`stage_a` doubles `x`, `stage_b` adds `a.y` and `x`) against the
//! equivalent calls to two single-graph sessions made by hand: the cost of the pipeline's wiring (input lists, buffer
//! hand-over, output copy) over the graphs themselves. One intra-op thread. Prints mean, p50 and p95 per call over
//! 100000 timed calls.
//!
//! Gated on `OMUZAKI_ORT_CACHE`, as the tests; prints a line and exits when it is unset.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss)] // a bench that cannot run should stop

use std::collections::BTreeMap;
use std::time::Instant;

use omuzaki_ai_api::{DType, Dim, Runtime as _, SessionOptions, Signature, Stage, TensorBuf, TensorRef, TensorSpec};
use omuzaki_ai_ort::OrtRuntime;

#[path = "../tests/common/mod.rs"]
mod common;

const CALLS: usize = 100_000;

fn port(name: &str) -> TensorSpec {
    TensorSpec::new(name, DType::F32, [Dim::Fixed(1), Dim::Fixed(4)])
}

fn stage(name: &str, file: &str, inputs: &[(&str, &str)]) -> Stage {
    Stage {
        name: name.to_owned(),
        file: file.to_owned(),
        inputs: inputs.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect(),
    }
}

/// Times `CALLS` calls of `body` and prints its mean, p50 and p95 in microseconds.
fn time(label: &str, mut body: impl FnMut()) -> f64 {
    for _ in 0..1000 {
        body(); // warm-up
    }
    let mut micros: Vec<f64> = (0..CALLS)
        .map(|_| {
            let started = Instant::now();
            body();
            started.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    micros.sort_by(f64::total_cmp);
    let mean = micros.iter().sum::<f64>() / micros.len() as f64;
    eprintln!(
        "{label}: mean {mean:.2} us, p50 {:.2} us, p95 {:.2} us",
        micros[micros.len() / 2],
        micros[micros.len() * 95 / 100]
    );
    mean
}

fn main() {
    let Some(natives) = common::cache("the pipeline bench") else { return };
    let files = [
        common::local("a.onnx", &common::fixture("stage_a.onnx")),
        common::local("b.onnx", &common::fixture("stage_b.onnx")),
    ];
    let options = SessionOptions::new(Vec::new(), 1);
    let no_io = BTreeMap::new();
    let stages = [stage("a", "a.onnx", &[("x", "x")]), stage("b", "b.onnx", &[("p", "a.y"), ("q", "x")])];

    let slot = Signature { inputs: vec![port("x")], outputs: vec![port("z")] };
    let io = BTreeMap::from([("z".to_owned(), "b.z".to_owned())]);
    let mut pipeline =
        common::with_source(&files, &stages, &natives, |s| OrtRuntime.load(s, &slot, &io, &options)).unwrap();

    // The same two graphs as single sessions, wired by the caller.
    let single = |file: &str, slot: &Signature| {
        common::with_source(&files, &[], &natives, |s| {
            let one = [common::local("model.onnx", s.file(file).unwrap())];
            common::with_source(&one, &[], &natives, |s| OrtRuntime.load(s, slot, &no_io, &options))
        })
        .unwrap()
    };
    let mut first = single("a.onnx", &Signature { inputs: vec![port("x")], outputs: vec![port("y")] });
    let mut second = single("b.onnx", &Signature { inputs: vec![port("p"), port("q")], outputs: vec![port("z")] });

    let x = [1.0f32, 2.0, 3.0, 4.0];
    let input = [TensorRef::f32(&x, &[1, 4])];
    let mut out = TensorBuf::for_outputs(pipeline.signature());
    let piped = time("pipeline (2 stages), 100000 calls", || pipeline.run(&input, &mut out).unwrap());

    let mut mid = TensorBuf::for_outputs(first.signature());
    let mut end = TensorBuf::for_outputs(second.signature());
    let by_hand = time("two single sessions by hand, 100000 calls", || {
        first.run(&input, &mut mid).unwrap();
        second.run(&[mid[0].as_ref(), input[0]], &mut end).unwrap();
    });
    eprintln!("pipeline overhead over the hand-wired calls: {:+.2} us per call", piped - by_hand);
}
