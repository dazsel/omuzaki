//! Benchmark: the whole mouth section with its interior, per pose, on a warm draw list: a grin with fangs, a `bleh`
//! with the tongue out, a wide `ee` with clenched teeth, and the closed line for reference; and the switch from the
//! closed line to the open shape (openings 0.06, 0.12 and 0.2, a smile and fangs on), where the rim, the lens and the
//! closed and open fangs are all drawn at once. The real-time budget is the whole face at 0.5 ms (spec v3 §10); the
//! mouth is one section of it.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_core::time::Seconds;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceMapping, FaceProfile, FaceView, MouthParams};
use omuzaki_face_mock::mock_projection;

/// One benchmarked mouth: a name and how it differs from the rest face.
type Pose = (&'static str, fn(&mut MouthParams));

fn bench_interior(c: &mut Criterion) {
    let mut profile = FaceProfile::default();
    profile.mouth.lips.strength = 1.0;
    let poses: [Pose; 7] = [
        ("closed-line", |_| {}),
        ("grin-fangs", |m| (m.open, m.smile_left, m.smile_right, m.fangs, m.teeth_upper) = (0.7, 1.2, 1.2, 1.0, 0.8)),
        ("bleh", |m| (m.open, m.round, m.tongue, m.teeth_upper) = (0.5, 0.6, 1.0, 0.4)),
        ("ee-clenched", |m| (m.open, m.wide, m.teeth_upper, m.teeth_lower) = (0.3, 1.3, 1.0, 1.0)),
        ("switch-0.06", |m| (m.open, m.smile_left, m.smile_right, m.fangs) = (0.06, 1.0, 1.0, 1.0)),
        ("switch-0.12", |m| (m.open, m.smile_left, m.smile_right, m.fangs) = (0.12, 1.0, 1.0, 1.0)),
        ("switch-0.2", |m| (m.open, m.smile_left, m.smile_right, m.fangs) = (0.2, 1.0, 1.0, 1.0)),
    ];
    let mut group = c.benchmark_group("mouth_interior");
    for (name, edit) in poses {
        let mut params = FaceMapping::default().rest_params();
        edit(&mut params.mouth);
        let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
        let mut drawing = FaceDrawing::new();
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        // Warm the lists to their working size before measuring.
        painter.paint_section(Section::Mouth, &input, &mut drawing).ok();
        group.bench_function(name, |b| {
            b.iter(|| black_box(painter.paint_section(Section::Mouth, black_box(&input), &mut drawing)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_interior);
criterion_main!(benches);
