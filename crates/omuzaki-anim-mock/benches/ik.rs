//! Benchmark for the stand-in IK stage: four limbs plus look-at on the 55-bone fixture humanoid, the full per-frame
//! load of an `IkSolver`. Run with `cargo bench -p omuzaki-anim-mock --bench ik`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{IkGoals, IkSolver, Limb, LookAt};
use omuzaki_anim_mock::fixtures::humanoid_skeleton;
use omuzaki_anim_mock::ik::MockIk;
use omuzaki_core::glam::Vec3;

fn limbs_and_look_at(c: &mut Criterion) {
    let Ok(skeleton) = humanoid_skeleton() else { return };
    let limb =
        |x: f32, y: f32| Some(Limb { target: Vec3::new(x, y, 0.3), pole: Vec3::new(x, y - 0.3, -0.5), weight: 0.8 });
    let goals = IkGoals {
        look_at: Some(LookAt { target: Vec3::new(0.2, 1.5, 2.0), weight: 1.0, eye_share: 0.4 }),
        left_hand: limb(0.4, 1.1),
        right_hand: limb(-0.4, 1.1),
        left_foot: limb(0.1, 0.1),
        right_foot: limb(-0.1, 0.1),
    };
    let rest = skeleton.rest_pose();
    let mut pose = rest.clone();
    let mut solver = MockIk::new();
    c.bench_function("ik/four_limbs_and_look_at", |b| {
        b.iter(|| {
            pose.clone_from(&rest);
            solver.solve(black_box(&skeleton), black_box(&goals), &mut pose);
        });
    });
}

criterion_group!(benches, limbs_and_look_at);
criterion_main!(benches);
