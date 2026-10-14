//! Benchmark for the gaze solver's per-frame cost (spec §10). Run with `cargo bench -p omuzaki-anim --bench gaze`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::gaze::{GazeKind, GazeSolver, Resolved};
use omuzaki_anim_api::{Bus, Tick};
use omuzaki_core::glam::{Mat4, Vec3};

fn gaze_frame(c: &mut Criterion) {
    let mut solver = GazeSolver::default();
    let mut bus = Bus::default();
    let eye_frame = Mat4::from_rotation_y(0.3) * Mat4::from_translation(Vec3::new(0.0, 0.22, 0.0));
    let target = Some(Resolved { position: Vec3::new(0.6, 0.1, 1.5), kind: GazeKind::Follow });
    let mut index = 0_u64;
    c.bench_function("gaze: one frame at 60 fps, a near target", |b| {
        b.iter(|| {
            let frame = solver.step(Tick::at_rate(index, 60.0), black_box(eye_frame), black_box(target), &mut bus);
            index += 1;
            black_box(frame);
        });
    });
}

criterion_group!(benches, gaze_frame);
criterion_main!(benches);
