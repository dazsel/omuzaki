//! Benchmark: the mouth shape piece alone, per shape, on a warm draw list: the closed line, the kiss and the
//! open shape. The real-time budget is the whole face at 0.5 ms (spec v3 §10); the mouth is one section of it.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::Seconds;
use omuzaki_face::{MouthPiece, RealPainter};
use omuzaki_face_api::{DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceMapping, FaceProfile, FaceView, MouthParams};
use omuzaki_face_mock::mock_projection;

/// One benchmarked mouth: a name and how it differs from the rest face.
type Shape = (&'static str, fn(&mut MouthParams));

fn bench_shapes(c: &mut Criterion) {
    let profile = FaceProfile::default();
    let shapes: [Shape; 4] = [
        ("closed-line", |_| {}),
        ("kiss", |m| m.pucker = 0.9),
        ("open-aa", |m| m.open = 0.85),
        ("open-round-smirk", |m| (m.open, m.round, m.smile_left, m.smile_right) = (0.5, 1.0, 1.5, -0.5)),
    ];
    let mut group = c.benchmark_group("mouth_shape");
    for (name, edit) in shapes {
        let mut params = FaceMapping::default().rest_params();
        edit(&mut params.mouth);
        let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
        let mut drawing = FaceDrawing::new();
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        // Warm the lists to their working size before measuring.
        painter.paint_mouth_piece(MouthPiece::Shape, &input, &mut drawing).ok();
        group.bench_function(name, |b| {
            b.iter(|| black_box(painter.paint_mouth_piece(MouthPiece::Shape, black_box(&input), &mut drawing)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_shapes);
criterion_main!(benches);
