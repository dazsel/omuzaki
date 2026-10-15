//! Node constraints against hand-computed expectations: the issue's acceptance cases, tilted rests, ordering, and
//! determinism.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use omuzaki_anim_api::{
    Axis, BoneConstraint, BoneDef, BoneId, ConstraintError, ConstraintKind, ConstraintSet, Pose, SignedAxis, Skeleton,
    Transform,
};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_rig::constraint::VrmConstraintSolver;

#[path = "common/mod.rs"]
mod common;

const EPS: f32 = 1e-6;

fn bone(name: &str, parent: Option<u16>, translation: Vec3, rotation: Quat) -> BoneDef {
    BoneDef { name: name.into(), parent: parent.map(BoneId), rest: Transform { translation, rotation }, humanoid: None }
}

/// A root, then a destination (1) and a source (2) at the given rest rotations, both under the root.
fn pair(dst_rest: Quat, src_rest: Quat, src_at: Vec3) -> Skeleton {
    Skeleton::generic(vec![
        bone("root", None, Vec3::ZERO, Quat::IDENTITY),
        bone("dst", Some(0), Vec3::ZERO, dst_rest),
        bone("src", Some(0), src_at, src_rest),
    ])
    .unwrap()
}

fn one(kind: ConstraintKind, weight: f32) -> ConstraintSet {
    ConstraintSet { constraints: vec![BoneConstraint { bone: BoneId(1), source: BoneId(2), kind, weight }] }
}

/// Solves `set` on a rest pose whose source (bone 2) has local rotation `src`; returns the destination's rotation.
fn solve(sk: &Skeleton, set: &ConstraintSet, src: Quat) -> Quat {
    let mut pose = sk.rest_pose();
    pose.local[2].rotation = src;
    VrmConstraintSolver::new(set, sk).unwrap().apply(sk, &mut pose);
    pose.local[1].rotation
}

fn near(a: Quat, b: Quat) -> bool {
    common::same_rotation(a, b, EPS)
}

fn twist_about_x(q: Quat) -> f32 {
    let q = if q.w < 0.0 { -q } else { q };
    2.0 * q.x.atan2(q.w)
}

#[test]
fn rotation_copies_the_change_from_rest() {
    let sk = pair(Quat::IDENTITY, Quat::IDENTITY, Vec3::Y);
    let src = Quat::from_rotation_x(90_f32.to_radians());
    assert!(near(solve(&sk, &one(ConstraintKind::Rotation, 1.0), src), src));
    assert!(near(solve(&sk, &one(ConstraintKind::Rotation, 0.5), src), Quat::from_rotation_x(45_f32.to_radians())));
    assert!(near(solve(&sk, &one(ConstraintKind::Rotation, 0.0), src), Quat::IDENTITY));
}

#[test]
fn rotation_with_tilted_rests_is_local_to_local() {
    let d_rest = Quat::from_euler(omuzaki_core::glam::EulerRot::YXZ, 0.4, -0.2, 0.7);
    let s_rest = Quat::from_euler(omuzaki_core::glam::EulerRot::YXZ, -0.9, 0.3, 0.1);
    let sk = pair(d_rest, s_rest, Vec3::Y);
    let delta = Quat::from_rotation_z(0.6);
    let got = solve(&sk, &one(ConstraintKind::Rotation, 1.0), s_rest * delta);
    assert!(near(got, d_rest * delta));
}

#[test]
fn roll_transfers_the_twist_scaled_by_weight() {
    let sk = pair(Quat::IDENTITY, Quat::IDENTITY, Vec3::Y);
    let roll = ConstraintKind::Roll { axis: Axis::X };
    let twist = Quat::from_rotation_x(80_f32.to_radians());
    for weight in [1.0, 0.5, 0.25] {
        let got = solve(&sk, &one(roll, weight), twist);
        assert!((twist_about_x(got) - 80_f32.to_radians() * weight).abs() < EPS, "weight {weight}");
        assert!(got.y.abs() < EPS && got.z.abs() < EPS, "a pure twist stays about X");
    }
}

#[test]
fn roll_ignores_swing_and_keeps_only_the_twist_of_a_mixed_turn() {
    let sk = pair(Quat::IDENTITY, Quat::IDENTITY, Vec3::Y);
    let roll = ConstraintKind::Roll { axis: Axis::X };
    let swing = Quat::from_rotation_y(30_f32.to_radians());
    assert!(near(solve(&sk, &one(roll, 1.0), swing), Quat::IDENTITY));
    // Swing then twist about the swung axis: the destination gets the twist, whichever order they were composed in.
    let mixed = swing * Quat::from_rotation_x(50_f32.to_radians());
    let got = solve(&sk, &one(roll, 1.0), mixed);
    assert!((twist_about_x(got) - 50_f32.to_radians()).abs() < EPS);
    assert!(got.y.abs() < EPS && got.z.abs() < EPS);
}

#[test]
fn roll_about_other_axes_and_tilted_destination() {
    let d_rest = Quat::from_rotation_z(0.5);
    let sk = pair(d_rest, Quat::IDENTITY, Vec3::Y);
    // The destination's own Y axis, in the parent's frame, is d_rest * Y; a source turn about it is the roll.
    let around = Quat::from_axis_angle(d_rest * Vec3::Y, 0.8);
    let got = solve(&sk, &one(ConstraintKind::Roll { axis: Axis::Y }, 1.0), around);
    assert!(near(got, d_rest * Quat::from_rotation_y(0.8)));
}

#[test]
fn aim_turns_the_axis_toward_the_source() {
    let sk = pair(Quat::IDENTITY, Quat::IDENTITY, Vec3::Y);
    let aim = |axis, weight| one(ConstraintKind::Aim { axis }, weight);
    let got = solve(&sk, &aim(SignedAxis::PositiveX, 1.0), Quat::IDENTITY);
    assert!((got * Vec3::X - Vec3::Y).length() < EPS);
    let got = solve(&sk, &aim(SignedAxis::NegativeX, 1.0), Quat::IDENTITY);
    assert!((got * Vec3::NEG_X - Vec3::Y).length() < EPS);
    assert!(near(solve(&sk, &aim(SignedAxis::PositiveX, 0.0), Quat::IDENTITY), Quat::IDENTITY));
    let half = solve(&sk, &aim(SignedAxis::PositiveX, 0.5), Quat::IDENTITY);
    assert!(near(half, Quat::from_rotation_z(45_f32.to_radians())));
}

#[test]
fn aim_works_through_a_rotated_parent_and_ignores_the_destinations_own_pose() {
    // root turned 90 degrees about Z: the destination's local +X axis is then world +Y, and the source is at local
    // (0,1,0), which is world (-1,0,0).
    let sk = Skeleton::generic(vec![
        bone("root", None, Vec3::ZERO, Quat::from_rotation_z(90_f32.to_radians())),
        bone("dst", Some(0), Vec3::ZERO, Quat::IDENTITY),
        bone("src", Some(0), Vec3::Y, Quat::IDENTITY),
    ])
    .unwrap();
    let mut pose = sk.rest_pose();
    pose.local[1].rotation = Quat::from_rotation_y(1.0); // overwritten by the constraint
    let set = one(ConstraintKind::Aim { axis: SignedAxis::PositiveX }, 1.0);
    VrmConstraintSolver::new(&set, &sk).unwrap().apply(&sk, &mut pose);
    let mut world = Pose::IDENTITY;
    sk.world_pose(&pose, &mut world);
    let aimed = world.local[1].rotation * Vec3::X;
    assert!((aimed - Vec3::NEG_X).length() < 1e-5, "{aimed:?}");
}

#[test]
fn aim_with_the_source_on_the_destination_or_behind_it_is_well_defined() {
    let aim = one(ConstraintKind::Aim { axis: SignedAxis::PositiveX }, 1.0);
    let on_top = pair(Quat::IDENTITY, Quat::IDENTITY, Vec3::ZERO);
    assert!(near(solve(&on_top, &aim, Quat::IDENTITY), Quat::IDENTITY));
    let behind = pair(Quat::IDENTITY, Quat::IDENTITY, Vec3::NEG_X);
    let got = solve(&behind, &aim, Quat::IDENTITY);
    assert!((got * Vec3::X - Vec3::NEG_X).length() < EPS);
    assert!((got.length() - 1.0).abs() < EPS);
}

fn chain_skeleton() -> Skeleton {
    let mut bones = vec![bone("root", None, Vec3::ZERO, Quat::IDENTITY)];
    bones.extend((1..5).map(|i| bone(&format!("b{i}"), Some(0), Vec3::ZERO, Quat::IDENTITY)));
    Skeleton::generic(bones).unwrap()
}

fn rot(bone: u16, source: u16) -> BoneConstraint {
    BoneConstraint { bone: BoneId(bone), source: BoneId(source), kind: ConstraintKind::Rotation, weight: 1.0 }
}

#[test]
fn a_constraint_runs_after_the_one_that_drives_its_source() {
    let sk = chain_skeleton();
    let turn = Quat::from_rotation_x(0.5);
    // 1 follows 2 follows 3 (unconstrained). Declared in the worst order.
    for order in [[rot(1, 2), rot(2, 3)], [rot(2, 3), rot(1, 2)]] {
        let set = ConstraintSet { constraints: order.to_vec() };
        let solver = VrmConstraintSolver::new(&set, &sk).unwrap();
        let mut pose = sk.rest_pose();
        pose.local[3].rotation = turn;
        solver.apply(&sk, &mut pose);
        assert!(near(pose.local[2].rotation, turn));
        assert!(near(pose.local[1].rotation, turn), "bone 1 saw bone 2 before it was constrained");
    }
}

#[test]
fn constraints_that_drive_each_other_are_a_cycle_naming_both_bones() {
    let sk = chain_skeleton();
    let set = ConstraintSet { constraints: vec![rot(1, 2), rot(2, 1)] };
    let Err(ConstraintError::Cycle { mut bones }) = VrmConstraintSolver::new(&set, &sk) else {
        panic!("expected a cycle");
    };
    bones.sort_unstable();
    assert_eq!(bones, vec![1, 2]);
}

#[test]
fn bad_sets_are_rejected_by_new() {
    let sk = chain_skeleton();
    assert!(VrmConstraintSolver::new(&ConstraintSet { constraints: vec![rot(1, 1)] }, &sk).is_err());
    assert!(VrmConstraintSolver::new(&ConstraintSet { constraints: vec![rot(1, 2), rot(1, 3)] }, &sk).is_err());
    assert!(VrmConstraintSolver::new(&ConstraintSet { constraints: vec![rot(1, 99)] }, &sk).is_err());
}

/// 32 mixed constraints on bones 1..=32 of the 128-bone character; every fourth follows the constraint before it.
fn mixed_set() -> ConstraintSet {
    let constraints = (1_u16..=32)
        .map(|i| BoneConstraint {
            bone: BoneId(i),
            source: BoneId(if i % 4 == 0 { i - 1 } else { 64 + i }),
            kind: match i % 3 {
                0 => ConstraintKind::Roll { axis: Axis::X },
                1 => ConstraintKind::Aim { axis: SignedAxis::PositiveY },
                _ => ConstraintKind::Rotation,
            },
            weight: 0.25 + f32::from(i % 4) * 0.25,
        })
        .collect();
    ConstraintSet { constraints }
}

fn run_checksum() -> u64 {
    let ch = common::full_character(true);
    let solver = VrmConstraintSolver::new(&mixed_set(), &ch.skeleton).unwrap();
    let mut rng = Rng::new(5);
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for _ in 0..50 {
        let mut pose = ch.skeleton.rest_pose();
        for t in pose.local.iter_mut().take(ch.skeleton.len()) {
            t.rotation = Quat::from_euler(
                omuzaki_core::glam::EulerRot::YXZ,
                rng.range_f32(-1.0, 1.0),
                rng.range_f32(-1.0, 1.0),
                rng.range_f32(-1.0, 1.0),
            );
        }
        solver.apply(&ch.skeleton, &mut pose);
        for t in pose.local.iter().take(33) {
            for v in t.rotation.to_array() {
                hash = (hash ^ u64::from(v.to_bits())).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    hash
}

#[test]
fn two_runs_are_bit_identical() {
    assert_eq!(run_checksum(), run_checksum());
}

#[test]
fn outputs_are_unit_quaternions() {
    let ch = common::full_character(true);
    let solver = VrmConstraintSolver::new(&mixed_set(), &ch.skeleton).unwrap();
    let mut pose = ch.skeleton.rest_pose();
    for t in pose.local.iter_mut().take(ch.skeleton.len()) {
        t.rotation = Quat::from_rotation_x(2.0);
    }
    solver.apply(&ch.skeleton, &mut pose);
    for t in pose.local.iter().take(33) {
        assert!((t.rotation.length() - 1.0).abs() < 1e-5);
    }
}
