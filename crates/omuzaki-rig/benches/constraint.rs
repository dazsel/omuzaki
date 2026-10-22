//! Benchmark for the node-constraint stage: the budget is 32 mixed constraints on the 128-bone character in under 10
//! microseconds. Run with `cargo bench -p omuzaki-rig --bench constraint`.

#![allow(clippy::expect_used, clippy::cast_precision_loss)] // fixture failures should stop the run

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{Axis, BoneConstraint, BoneId, ConstraintKind, ConstraintSet, SignedAxis};
use omuzaki_core::glam::{EulerRot, Quat};
use omuzaki_rig::constraint::VrmConstraintSolver;

#[path = "../tests/common/mod.rs"]
mod common;

fn thirty_two_constraints(c: &mut Criterion) {
    let ch = common::full_character(true);
    let constraints = (1_u16..=32)
        .map(|i| BoneConstraint {
            bone: BoneId(64 + i),
            source: BoneId(if i % 4 == 0 { 63 + i } else { i }),
            kind: match i % 3 {
                0 => ConstraintKind::Roll { axis: Axis::X },
                1 => ConstraintKind::Aim { axis: SignedAxis::PositiveY },
                _ => ConstraintKind::Rotation,
            },
            weight: 0.5,
        })
        .collect();
    let solver = VrmConstraintSolver::new(&ConstraintSet { constraints }, &ch.skeleton).expect("the set is valid");
    let mut pose = ch.skeleton.rest_pose();
    for (i, t) in pose.local.iter_mut().take(ch.skeleton.len()).enumerate() {
        t.rotation = Quat::from_euler(EulerRot::YXZ, i as f32 * 0.1, 0.3, -0.2);
    }
    c.bench_function("node constraints, 32 mixed on 128 bones", |b| {
        b.iter(|| solver.apply(black_box(&ch.skeleton), black_box(&mut pose)));
    });
}

criterion_group!(benches, thirty_two_constraints);
criterion_main!(benches);
