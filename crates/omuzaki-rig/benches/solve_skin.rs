//! Benchmarks for the rig's per-frame work (budgets: spec §10, the whole CPU side is under 3% of an 8-core at 1080p30).
//! Run with `cargo bench -p omuzaki-rig --bench solve_skin`.
//!
//! The 128-joint skeleton is the size the task names: 55 humanoid bones, an armature root and 72 hair bones. The
//! 55-bone numbers are there to compare with the contract's own figures (`omuzaki-anim-api` benches).

#![allow(clippy::expect_used)] // a fixture that fails to build should stop the run with its reason

#[path = "../tests/common/mod.rs"]
mod common;

use std::hint::black_box;

use common::{character, full_character, random_pose};
use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::{
    BoneId, HumanoidBone, HumanoidPose, IkGoals, Limb, Pose, Rig, Skeleton, SpringChain, SpringParams, SpringSet, Tick,
};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::full::FullRig;
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

fn rig_and_pose(ch: &common::Character) -> (HumanoidRig, HumanoidPose) {
    let rig = HumanoidRig::new(RigSkeleton::new(ch.skeleton.clone()));
    (rig, random_pose(&mut Rng::new(1), 0.6))
}

fn bench_size(c: &mut Criterion, label: &str, ch: &common::Character) {
    let (mut rig, pose) = rig_and_pose(ch);
    let mut skinner = Skinner::new(rig.rig_skeleton().clone());
    let (mut local, mut back) = (Pose::IDENTITY, HumanoidPose::REST);
    let mut skin = vec![Mat4::IDENTITY; ch.skeleton.len()];

    c.bench_function(&format!("apply_humanoid ({label})"), |b| {
        b.iter(|| rig.apply_humanoid(black_box(&pose), &mut local));
    });
    rig.apply_humanoid(&pose, &mut local);
    c.bench_function(&format!("to_humanoid ({label})"), |b| b.iter(|| rig.to_humanoid(black_box(&local), &mut back)));
    c.bench_function(&format!("skin matrices ({label})"), |b| {
        b.iter(|| skinner.skin(black_box(&local), &mut skin));
    });

    // The whole rig stage of a frame: solve the pose, then fill the scene's joint palette.
    let mut scene = Scene::new(Camera::default());
    let tick = Tick::at_rate(0, 30.0);
    c.bench_function(&format!("frame: solve + skin into the scene ({label})"), |b| {
        b.iter(|| {
            scene.joints.clear();
            rig.solve(tick, black_box(&pose), &mut local);
            skinner.push(&local, &mut scene.joints)
        });
    });
}

fn rig(c: &mut Criterion) {
    let all: Vec<_> = HumanoidBone::ALL.to_vec();
    bench_size(c, "56 joints", &character(&all, true, 0, 0, 7));
    bench_size(c, "128 joints", &full_character(true));
}

/// The full rig on the 128-joint character: humanoid + IK on four limbs + 72 spring bones + skinning. `separate` runs
/// the stages as a caller would without `FullRig` (a skinner that does its own forward-kinematics pass, then the
/// caller's own pass for the world pose it also needs); `shared` is `solve_and_skin`, one pass for both.
fn full(c: &mut Criterion) {
    let ch = full_character(true);
    let pose = random_pose(&mut Rng::new(1), 0.6);
    let mut hair = SpringSet { chains: Vec::new(), groups: Vec::new() };
    // The fixture appends its 9 hair chains of 8 bones one after another, after the humanoid bones.
    let hair_bones: Vec<BoneId> = (0..ch.skeleton.len())
        .filter(|&i| ch.skeleton.bones()[i].humanoid.is_none() && ch.skeleton.bones()[i].parent.is_some())
        .map(|i| BoneId(u16::try_from(i).expect("128 bones")))
        .collect();
    for (i, chain) in hair_bones.chunks(8).enumerate() {
        let params = SpringParams { stiffness: 1.0, gravity_power: 1.0, ..SpringParams::default() };
        hair.chains.push(SpringChain::tapered(format!("hair_{i}"), chain, params, params));
    }
    let skeleton = RigSkeleton::new(ch.skeleton.clone());
    let mut rig = FullRig::new(skeleton.clone(), &hair).expect("the fixture's hair chains are valid");
    let mut rig_no_goals = FullRig::new(skeleton.clone(), &hair).expect("the fixture's hair chains are valid");
    let mut reference = HumanoidRig::new(skeleton.clone());
    let mut skinner = Skinner::new(skeleton);
    let goals = goals(&ch.skeleton);
    rig.set_goals(&goals);
    let (mut local, mut world) = (Pose::IDENTITY, Pose::IDENTITY);
    let mut joints = vec![Mat4::IDENTITY; ch.skeleton.len()];
    let tick = Tick::at_rate(0, 60.0);

    c.bench_function("full rig: solve + skin, no goals (128 joints)", |b| {
        b.iter(|| rig_no_goals.solve_and_skin(tick, black_box(&pose), &mut local, &mut joints));
    });
    c.bench_function("full rig: solve + skin, shared FK (128 joints)", |b| {
        b.iter(|| rig.solve_and_skin(tick, black_box(&pose), &mut local, &mut joints));
    });
    c.bench_function("full rig: solve, skin and world pose, separate passes (128 joints)", |b| {
        b.iter(|| {
            rig.solve(tick, black_box(&pose), &mut local);
            skinner.skin(&local, &mut joints);
            ch.skeleton.world_pose(&local, &mut world);
        });
    });
    c.bench_function("humanoid only: solve + skin (128 joints)", |b| {
        b.iter(|| {
            reference.solve(tick, black_box(&pose), &mut local);
            skinner.skin(&local, &mut joints);
        });
    });
}

/// Reaching goals for all four limbs, a little in front of each limb's root.
fn goals(skeleton: &Skeleton) -> IkGoals {
    let rest = skeleton.rest_pose();
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&rest, &mut world);
    let limb = |root: HumanoidBone| {
        let at = skeleton.humanoid_bone(root).map_or(Vec3::ZERO, |id| world.local[id.index()].translation);
        Limb { target: at + Vec3::new(0.1, -0.1, 0.15), pole: at + Vec3::Z, weight: 1.0 }
    };
    IkGoals {
        look_at: None,
        left_hand: Some(limb(HumanoidBone::LeftUpperArm)),
        right_hand: Some(limb(HumanoidBone::RightUpperArm)),
        left_foot: Some(limb(HumanoidBone::LeftUpperLeg)),
        right_foot: Some(limb(HumanoidBone::RightUpperLeg)),
    }
}

criterion_group!(benches, rig, full);
criterion_main!(benches);
