//! The likeness measures' budgets (design 7.4): `measure.face_outline` with the spacing on a synthetic face of about
//! 1024² at an IOD of 256 px (decode excluded, budget 30 ms), and `measure.likeness` over two such faces (70 ms). Run
//! with `cargo bench -p omuzaki-measure --bench face`.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "a benchmark's setup")]

#[path = "../tests/support/mod.rs"]
mod support;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_measure::face::likeness::{Compare, Tolerances, likeness};
use omuzaki_measure::face::spacing::spacing;
use support::synth::{FACE_A, FACE_C, Shot, Style, draw};
use support::{Measured, true_landmarks};

fn measures(c: &mut Criterion) {
    let reference = draw(&FACE_A, Style::Drawing, Shot::new(256.0));
    let candidate = draw(&FACE_C, Style::RenderLike, Shot { iod_px: 256.0, roll_deg: 3.0, offset: [0.4, 0.2] });
    let (lr, lc) = (true_landmarks(&reference), true_landmarks(&candidate));
    c.bench_function("measure/face_outline_and_spacing_1024", |b| {
        b.iter(|| {
            let m = support::measure(black_box(&candidate), lc, true).unwrap();
            black_box(spacing(&m))
        });
    });
    c.bench_function("measure/likeness_two_faces_1024", |b| {
        b.iter(|| {
            let r = Measured::new(black_box(&reference), lr, true).unwrap();
            let k = Measured::new(black_box(&candidate), lc, true).unwrap();
            black_box(likeness(r.side(), k.side(), Tolerances::preset(Compare::CrossLook)).unwrap().score)
        });
    });
}

criterion_group!(benches, measures);
criterion_main!(benches);
