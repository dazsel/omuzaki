//! Benchmark for putting a hand shape on a pose, the per-frame cost of cues, clips and hand tracking (spec §10: a frame's
//! whole CPU side is a small share of a millisecond). Run with `cargo bench -p omuzaki-anim --bench hands`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::hands::{HandLibrary, HandRest, HandShape, Side, apply};
use omuzaki_anim_api::HumanoidPose;

fn hands(c: &mut Criterion) {
    let library = HandLibrary::builtin().unwrap_or_else(|e| panic!("built-in hand shapes: {e}"));
    let fist = library.get("fist").unwrap_or_else(|| panic!("the fist shape"));
    let rest = HandRest::nominal();
    let mut pose = HumanoidPose::REST;
    c.bench_function("hands: a shape on both hands", |b| {
        b.iter(|| {
            apply(black_box(&mut pose), Side::Left, black_box(&rest), black_box(fist), 0.8);
            apply(black_box(&mut pose), Side::Right, black_box(&rest), black_box(fist), 0.8);
        });
    });

    // A tracked hand: the shape is built from five curls first, as every tracker frame does.
    let mut pose = HumanoidPose::REST;
    let mut phase = 0.0_f32;
    c.bench_function("hands: tracked curls on both hands", |b| {
        b.iter(|| {
            phase = (phase + 0.01).fract();
            let tracked = HandShape::from_curls(black_box([phase, 1.0 - phase, phase, 0.5, 0.2]));
            if let Ok(shape) = tracked {
                apply(&mut pose, Side::Left, &rest, &shape, 1.0);
                apply(&mut pose, Side::Right, &rest, &shape, 1.0);
            }
        });
    });
}

criterion_group!(benches, hands);
criterion_main!(benches);
