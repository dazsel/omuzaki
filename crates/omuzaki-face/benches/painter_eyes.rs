//! Benchmark: the eyes section of the real painter (aperture and eye white; later pieces add to
//! what it measures). Face-paint budget: 0.5 ms for the whole face (spec v3 §10).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::Seconds;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, EyeParams, FaceDrawing, FaceInput, FaceMapping, FaceProfile, FaceView, PaintParams,
};
use omuzaki_face_mock::mock_projection;

fn face(open: f32, close: f32) -> PaintParams {
    let eye = EyeParams { open, close, arc: 0.0, lower_lid: 0.2 };
    PaintParams { left_eye: eye, right_eye: eye, eye_tilt: 0.4, ..FaceMapping::default().rest_params() }
}

fn bench_eyes(c: &mut Criterion) {
    let profile = FaceProfile::default();
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let mut drawing = FaceDrawing::new();
    let mut group = c.benchmark_group("painter_eyes");
    for (name, params) in [("open", face(1.0, 0.0)), ("wide", face(1.9, 0.0)), ("shut", face(1.0, 1.0))] {
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        group
            .bench_function(name, |b| b.iter(|| black_box(painter.paint_section(Section::Eyes, &input, &mut drawing))));
    }
    group.finish();
}

criterion_group!(benches, bench_eyes);
criterion_main!(benches);
