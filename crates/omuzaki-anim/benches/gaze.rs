//! Benchmark for the gaze solver's per-frame cost (spec §10). Run with `cargo bench -p omuzaki-anim --bench gaze`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim::gaze::{
    EntityId, GazeCoordinator, GazeKind, GazeScene, GazeSolver, GazeTarget, HeadShare, Resolved, highlight_shift,
};
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

struct Scene;

impl GazeScene for Scene {
    fn camera_position(&self) -> Option<Vec3> {
        Some(Vec3::new(0.0, 0.0, 2.0))
    }

    fn entity_position(&self, _: EntityId) -> Option<Vec3> {
        None
    }
}

fn coordinated_frame(c: &mut Criterion) {
    let mut gaze = GazeCoordinator::new(GazeSolver::new(0.032), HeadShare::default(), 1);
    let mut index = 0_u64;
    // The target swings between 35 degrees right and 40 degrees left every second, so the head waits and turns.
    let targets = [Vec3::new(-1.15, 0.0, 1.64), Vec3::new(1.29, 0.0, 1.53)];
    c.bench_function("gaze: one coordinated frame (eyes and head) at 60 fps", |b| {
        b.iter(|| {
            let mut bus = Bus::default();
            let target = GazeTarget::Point(targets[usize::from((index / 60) % 2 == 1)]);
            let turn = gaze.step(Tick::at_rate(index, 60.0), Mat4::IDENTITY, black_box(target), &Scene, &mut bus);
            index += 1;
            black_box(turn);
        });
    });
    c.bench_function("gaze: the highlight shift", |b| {
        b.iter(|| black_box(highlight_shift(black_box(Vec3::new(0.4, 0.2, 1.8)))));
    });
}

criterion_group!(benches, gaze_frame, coordinated_frame);
criterion_main!(benches);
