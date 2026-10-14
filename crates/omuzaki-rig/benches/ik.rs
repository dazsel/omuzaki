//! Benchmarks for the IK solvers: the two-bone budget is 2 microseconds per limb; look-at is one goal per frame. Run with
//! `cargo bench -p omuzaki-rig --bench ik`.

#![allow(clippy::expect_used, clippy::cast_precision_loss)] // fixture failures should stop the run; small loop counters

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::LookAt;
use omuzaki_anim_api::{HumanoidBone, IkGoals, IkSolver, Limb};
use omuzaki_core::glam::Vec3;
use omuzaki_rig::ik::TwoBoneIk;
use omuzaki_rig::ik::feet::{Config, FootPlanting};
use omuzaki_rig::ik::look_at::LookAtIk;

#[path = "../tests/common/mod.rs"]
mod common;

fn two_bone(c: &mut Criterion) {
    let ch = common::full_character(true);
    let rest = ch.skeleton.rest_pose();
    let hand = ch.skeleton.humanoid_bone(HumanoidBone::LeftUpperArm).expect("the fixture has arms");
    let shoulder = ch.rest_pos[hand.index()];
    let limb = |target: Vec3, pole: Vec3| Some(Limb { target, pole, weight: 1.0 });
    let one = IkGoals { left_hand: limb(shoulder + Vec3::new(0.2, -0.3, 0.3), shoulder - Vec3::Z), ..IkGoals::NONE };
    let four = IkGoals {
        left_hand: one.left_hand,
        right_hand: limb(shoulder + Vec3::new(-0.6, -0.2, 0.3), shoulder - Vec3::Z),
        left_foot: limb(Vec3::new(0.1, 0.1, 0.2), Vec3::new(0.1, 0.5, 1.0)),
        right_foot: limb(Vec3::new(-0.1, 0.0, 0.1), Vec3::new(-0.1, 0.5, 1.0)),
        ..IkGoals::NONE
    };

    let mut ik = TwoBoneIk::new();
    for (name, goals) in [("one limb", one), ("four limbs", four)] {
        let mut pose = rest.clone();
        c.bench_function(&format!("two-bone IK, {name}"), |b| {
            b.iter(|| {
                pose.clone_from(&rest);
                ik.solve(&ch.skeleton, black_box(&goals), black_box(&mut pose));
            });
        });
    }
}

/// Foot planting for two legs on a walking character: both feet locked, the hips dropped a little to reach them.
fn foot_planting(c: &mut Criterion) {
    let ch = common::full_character(true);
    let rest = ch.skeleton.rest_pose();
    let mut planter = FootPlanting::new(&ch.skeleton, Config::default()).expect("the fixture is a humanoid");
    planter.set_contacts([Some(true), Some(true)]);
    let weight = Some(Limb { target: Vec3::ZERO, pole: Vec3::ZERO, weight: 1.0 });
    let goals = IkGoals { left_foot: weight, right_foot: weight, ..IkGoals::NONE };
    let mut pose = rest.clone();
    c.bench_function("foot planting, two legs", |b| {
        b.iter(|| {
            pose.clone_from(&rest);
            planter.solve(&ch.skeleton, black_box(&goals), black_box(&mut pose));
        });
    });
}

/// Look-at on the 128-joint skeleton: chest, neck, head and both eyes, with the eyes taking half the turn.
fn look_at(c: &mut Criterion) {
    let ch = common::full_character(true);
    let rest = ch.skeleton.rest_pose();
    let head = ch.rest_pos[ch.skeleton.humanoid_bone(HumanoidBone::Head).expect("the fixture has a head").index()];
    let goals = IkGoals {
        look_at: Some(LookAt { target: head + Vec3::new(0.8, 0.3, 1.5), weight: 1.0, eye_share: 0.5 }),
        ..IkGoals::NONE
    };
    let mut ik = LookAtIk::default();
    let mut pose = rest.clone();
    c.bench_function("look-at IK, chest + neck + head + eyes", |b| {
        b.iter(|| {
            pose.clone_from(&rest);
            ik.solve(&ch.skeleton, black_box(&goals), black_box(&mut pose));
        });
    });
}

criterion_group!(benches, two_bone, foot_planting, look_at);
criterion_main!(benches);
