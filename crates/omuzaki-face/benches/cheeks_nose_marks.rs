//! Benchmark: the cheeks, nose and marks sections painting into warm draw lists (spec v3 §10 face-paint budget
//! 0.5 ms for the whole face). Soft and hatched blush, the nose, and a profile with all four kinds of mark.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::time::Seconds;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    BlushKind, DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceMark, FaceProfile, FaceView, MarkKind, NoseKind,
};
use omuzaki_face_mock::{mock_profile, mock_projection};

fn marked(profile: &mut FaceProfile) {
    let mark = |kind, at, seed, count| FaceMark { kind, at, seed, count, size: 0.02, ..FaceMark::default() };
    profile.marks = vec![
        mark(MarkKind::Freckles, [0.05, -0.09], 3, 40),
        mark(MarkKind::BeautyMark, [0.07, -0.13], 1, 0),
        mark(MarkKind::Paint, [-0.08, -0.1], 0, 0),
        mark(MarkKind::Scar, [-0.05, -0.05], 1, 0),
    ];
}

fn bench_sections(c: &mut Criterion) {
    let mut soft = mock_profile();
    soft.nose.kind = NoseKind::Tick;
    marked(&mut soft);
    let mut hatched = soft.clone();
    hatched.cheeks.kind = BlushKind::Hatched;
    // The prototype's resting blush: the neutral channels give none, which would skip the patches.
    let mut params = soft.mapping.apply(&ChannelBuf::NEUTRAL);
    params.blush = 0.75;

    let mut group = c.benchmark_group("face_paint_cheeks_nose_marks");
    let cases = [
        ("blush_soft", Section::Cheeks, &soft),
        ("blush_hatched", Section::Cheeks, &hatched),
        ("nose_tick", Section::Nose, &soft),
        ("marks_4_kinds", Section::Marks, &soft),
    ];
    for (name, section, profile) in cases {
        let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
        let input = FaceInput { profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        let mut out = FaceDrawing::default();
        // Warm the lists to working size, as the frame loop's would be.
        painter.paint_section(section, &input, &mut out).ok();
        group.bench_function(name, |b| {
            b.iter(|| {
                painter.paint_section(section, black_box(&input), &mut out).ok();
                black_box(&out);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_sections);
criterion_main!(benches);
