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

/// 32 mixed constraints on hair bones 65..=96 of the 128-bone character, following humanoid bones 1..=32; every
/// fourth follows the constraint before it instead. (An aim on a humanoid bone at a hair bone below it would be a
/// cycle: the aim would move its own source.)
fn mixed_set() -> ConstraintSet {
    let constraints = (1_u16..=32)
        .map(|i| BoneConstraint {
            bone: BoneId(64 + i),
            source: BoneId(if i % 4 == 0 { 63 + i } else { i }),
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

#[test]
fn a_half_turn_across_the_roll_axis_is_no_roll_and_a_tiny_roll_passes_through() {
    let sk = pair(Quat::IDENTITY, Quat::IDENTITY, Vec3::Y);
    let roll = ConstraintKind::Roll { axis: Axis::X };
    let got = solve(&sk, &one(roll, 1.0), Quat::from_rotation_y(std::f32::consts::PI));
    assert!(near(got, Quat::IDENTITY), "{got:?}");
    let got = solve(&sk, &one(roll, 0.5), Quat::from_rotation_x(1e-4));
    assert!((twist_about_x(got) - 0.5e-4).abs() < 1e-7, "{}", twist_about_x(got));
}

#[test]
fn small_aim_errors_are_corrected_and_the_result_is_continuous() {
    let aim = one(ConstraintKind::Aim { axis: SignedAxis::PositiveX }, 1.0);
    let mut last = 0.0_f32;
    for k in 1..=40_u16 {
        let y = f32::from(k) * 5e-5; // crosses the ~7e-4 rad threshold of a naive arc
        let sk = pair(Quat::IDENTITY, Quat::IDENTITY, Vec3::new(1.0, y, 0.0));
        let got = solve(&sk, &aim, Quat::IDENTITY);
        let want = Vec3::new(1.0, y, 0.0).normalize();
        assert!((got * Vec3::X - want).length() < 1e-6, "y = {y}");
        let angle = got.to_scaled_axis().length();
        assert!(angle > last, "the turn grows smoothly with the error");
        last = angle;
    }
}

// ---- The order of an aim against its ancestors' constraints (#1619) ----

/// Bones for the ordering cases, all generic:
/// - 0 root at the origin;
/// - 1 `p`, the aim source's parent, at (0, 1, 0); 2 `s`, the aim source, 1 m along `p`'s +Z;
/// - 3 `x`, a free bone that drives `p`;
/// - 4 `q`, the aim destination's parent, at (0.5, 0, 0); 5 `d`, the aim destination, 0.5 m below `q`;
/// - 6 `y`, a free bone that drives `q`.
fn aim_rig() -> Skeleton {
    Skeleton::generic(vec![
        bone("root", None, Vec3::ZERO, Quat::IDENTITY),
        bone("p", Some(0), Vec3::Y, Quat::IDENTITY),
        bone("s", Some(1), Vec3::Z, Quat::IDENTITY),
        bone("x", Some(0), Vec3::X, Quat::IDENTITY),
        bone("q", Some(0), Vec3::new(0.5, 0.0, 0.0), Quat::IDENTITY),
        bone("d", Some(4), Vec3::new(0.0, -0.5, 0.0), Quat::IDENTITY),
        bone("y", Some(0), Vec3::NEG_X, Quat::IDENTITY),
    ])
    .unwrap()
}

const P: u16 = 1;
const S: u16 = 2;
const X: u16 = 3;
const Q: u16 = 4;
const D: u16 = 5;
const Y: u16 = 6;

fn aim(bone: u16, source: u16) -> BoneConstraint {
    BoneConstraint {
        bone: BoneId(bone),
        source: BoneId(source),
        kind: ConstraintKind::Aim { axis: SignedAxis::PositiveY },
        weight: 1.0,
    }
}

/// The aim rig's pose with the free drivers `x` and `y` turned.
fn driven_pose(sk: &Skeleton) -> Pose {
    let mut pose = sk.rest_pose();
    pose.local[usize::from(X)].rotation = Quat::from_rotation_x(1.1);
    pose.local[usize::from(Y)].rotation = Quat::from_rotation_z(-0.9);
    pose
}

/// The angle, radians, between `d`'s +Y axis and the direction from `d` to `s`, computed in `f64` from the solved
/// world transforms (`acos` of an `f32` dot is useless below about 5e-4 rad).
fn aim_error(sk: &Skeleton, pose: &Pose) -> f64 {
    let mut world = Pose::default();
    sk.world_pose(pose, &mut world);
    let (d, s) = (world.local[usize::from(D)], world.local[usize::from(S)]);
    let axis = (d.rotation * Vec3::Y).as_dvec3();
    let to = (s.translation - d.translation).as_dvec3();
    axis.cross(to).length().atan2(axis.dot(to))
}

/// Applies `constraints` one at a time in the order given, each with its own solver: what running the set in that
/// order does.
fn apply_in_order(sk: &Skeleton, constraints: &[BoneConstraint], pose: &mut Pose) {
    for c in constraints {
        VrmConstraintSolver::new(&ConstraintSet { constraints: vec![*c] }, sk).unwrap().apply(sk, pose);
    }
}

/// The issue's case and its mirror: an aim declared before the constraint that turns an ancestor of its source (or
/// of its destination) runs second, and points at the source.
#[test]
fn an_aim_runs_after_the_constraints_on_its_sources_and_destinations_ancestors() {
    let sk = aim_rig();
    for (case, ancestor) in [("source's parent", rot(P, X)), ("destination's parent", rot(Q, Y))] {
        let constraints = vec![aim(D, S), ancestor];
        let solver = VrmConstraintSolver::new(&ConstraintSet { constraints: constraints.clone() }, &sk).unwrap();
        assert_eq!(solver.ordered(), &[ancestor, aim(D, S)], "{case}: the aim must run second");

        let mut pose = driven_pose(&sk);
        solver.apply(&sk, &mut pose);
        let error = aim_error(&sk, &pose);
        assert!(error < 1e-5, "{case}: the destination misses the source by {error} rad");

        // Run in declaration order, the aim points at where the source was: swapping the two fails the check above.
        let mut declared = driven_pose(&sk);
        apply_in_order(&sk, &constraints, &mut declared);
        assert!(aim_error(&sk, &declared) > 0.1, "{case}: the declaration order should miss");
    }
}

#[test]
fn a_loop_through_an_ancestor_is_a_cycle_naming_its_bones() {
    let sk = aim_rig();
    // The aim waits for `p` (its source's parent), and `p` follows the aim's destination.
    let set = ConstraintSet { constraints: vec![aim(D, S), rot(P, D)] };
    assert_eq!(VrmConstraintSolver::new(&set, &sk).err(), Some(ConstraintError::Cycle { bones: vec![D, P] }));
    // An aim at a bone below its own destination waits on itself.
    let set = ConstraintSet { constraints: vec![aim(P, S)] };
    assert_eq!(VrmConstraintSolver::new(&set, &sk).err(), Some(ConstraintError::Cycle { bones: vec![P] }));
    // `ConstraintSet::order` alone, without the skeleton, sees neither loop.
    assert!(ConstraintSet { constraints: vec![aim(D, S), rot(P, D)] }.order().is_ok());
}

/// A number in `0..n`, from `rng`.
fn below(rng: &mut Rng, n: u64) -> u64 {
    rng.next_u64() % n
}

/// A random set of constraints on the 128-bone character, of the given kinds, with no bone constrained twice.
fn random_set(rng: &mut Rng, kinds: &[ConstraintKind], len: usize, bones: u16) -> ConstraintSet {
    let mut free: Vec<u16> = (1..bones).collect();
    let mut constraints = Vec::new();
    while constraints.len() < len {
        let at = usize::try_from(below(rng, u64::try_from(free.len()).unwrap())).unwrap();
        let bone = free.swap_remove(at);
        let source = loop {
            let s = u16::try_from(1 + below(rng, u64::from(bones - 1))).unwrap();
            if s != bone {
                break s;
            }
        };
        let kind = kinds[usize::try_from(below(rng, kinds.len() as u64)).unwrap()];
        constraints.push(BoneConstraint { bone: BoneId(bone), source: BoneId(source), kind, weight: 0.7 });
    }
    ConstraintSet { constraints }
}

#[test]
fn roll_and_rotation_keep_the_source_only_order() {
    let ch = common::full_character(true);
    let bones = u16::try_from(ch.skeleton.len()).unwrap();
    let kinds = [ConstraintKind::Rotation, ConstraintKind::Roll { axis: Axis::Y }];
    let mut rng = Rng::new(1619);
    let mut checked = 0;
    for _ in 0..200 {
        let set = random_set(&mut rng, &kinds, 24, bones);
        let Ok(order) = set.order() else { continue };
        let solver = VrmConstraintSolver::new(&set, &ch.skeleton).unwrap();
        let want: Vec<BoneConstraint> = order.into_iter().map(|i| set.constraints[i]).collect();
        assert_eq!(solver.ordered(), &want[..]);
        checked += 1;
    }
    assert!(checked > 100, "only {checked} of the random sets had no cycle");
}

/// three-vrm 3.5.5's `VRMNodeConstraintManager.update`, transcribed: every constraint in declaration order, each
/// first processing (depth first) the constraints on its dependencies and on their ancestors from the root. An aim
/// depends on its source and its destination's parent, roll and rotation on their source. `None` where three-vrm
/// throws for a circular dependency.
fn three_vrm_order(sk: &Skeleton, constraints: &[BoneConstraint]) -> Option<Vec<usize>> {
    fn process(
        i: usize,
        sk: &Skeleton,
        constraints: &[BoneConstraint],
        tried: &mut Vec<bool>,
        done: &mut Vec<bool>,
        out: &mut Vec<usize>,
    ) -> Option<()> {
        if done[i] {
            return Some(());
        }
        if tried[i] {
            return None;
        }
        tried[i] = true;
        let c = constraints[i];
        let mut dependencies = vec![c.source];
        if matches!(c.kind, ConstraintKind::Aim { .. }) {
            dependencies.extend(sk.bones()[c.bone.index()].parent);
        }
        for dependency in dependencies {
            let mut from_root: Vec<BoneId> =
                std::iter::successors(Some(dependency), |b| sk.bones()[b.index()].parent).collect();
            from_root.reverse();
            for ancestor in from_root {
                if let Some(j) = constraints.iter().position(|d| d.bone == ancestor) {
                    process(j, sk, constraints, tried, done, out)?;
                }
            }
        }
        out.push(i);
        done[i] = true;
        Some(())
    }
    let (mut tried, mut done, mut out) = (vec![false; constraints.len()], vec![false; constraints.len()], Vec::new());
    for i in 0..constraints.len() {
        process(i, sk, constraints, &mut tried, &mut done, &mut out)?;
    }
    Some(out)
}

/// Parity with the reference runtime's order (Director, #2 6186668022): on the aim cases and on random sets of all
/// three kinds whose aims have constrained ancestors, running the set in three-vrm's order gives the solver's pose bit
/// for bit. The two orders may break ties differently, but only between constraints that don't read each other.
#[test]
fn the_order_gives_the_same_pose_as_three_vrms() {
    let sk = aim_rig();
    for constraints in [
        vec![aim(D, S), rot(P, X), rot(Q, Y)],
        vec![rot(Q, Y), aim(D, S), rot(P, X)],
        vec![aim(D, S), rot(S, X), rot(P, Y), rot(Q, X)],
    ] {
        let order = three_vrm_order(&sk, &constraints).unwrap();
        let reference: Vec<BoneConstraint> = order.into_iter().map(|i| constraints[i]).collect();
        let mut want = driven_pose(&sk);
        apply_in_order(&sk, &reference, &mut want);
        let mut got = driven_pose(&sk);
        VrmConstraintSolver::new(&ConstraintSet { constraints }, &sk).unwrap().apply(&sk, &mut got);
        assert_eq!(got.local, want.local);
    }

    let ch = common::full_character(true);
    let bones = u16::try_from(ch.skeleton.len()).unwrap();
    let kinds = [
        ConstraintKind::Rotation,
        ConstraintKind::Roll { axis: Axis::Z },
        ConstraintKind::Aim { axis: SignedAxis::NegativeZ },
    ];
    let mut rng = Rng::new(801);
    let mut pose = ch.skeleton.rest_pose();
    for (i, t) in pose.local.iter_mut().take(ch.skeleton.len()).enumerate() {
        #[allow(clippy::cast_precision_loss)] // a bone index
        let i = i as f32;
        t.rotation = Quat::from_euler(omuzaki_core::glam::EulerRot::YXZ, i * 0.1, 0.3, -0.2);
    }
    let (mut compared, mut rejected) = (0, 0);
    for _ in 0..600 {
        let set = random_set(&mut rng, &kinds, 12, bones);
        let ours = VrmConstraintSolver::new(&set, &ch.skeleton);
        // A loop through these edges is a loop for three-vrm too, which can also throw on a loop of its extra
        // roll and rotation ancestor edges, so only a set three-vrm accepts must be accepted here.
        let Some(order) = three_vrm_order(&ch.skeleton, &set.constraints) else {
            rejected += 1;
            continue;
        };
        let solver = ours.unwrap_or_else(|e| panic!("three-vrm runs this set but the solver refuses it: {e}"));
        let reference: Vec<BoneConstraint> = order.into_iter().map(|i| set.constraints[i]).collect();
        let (mut want, mut got) = (pose.clone(), pose.clone());
        apply_in_order(&ch.skeleton, &reference, &mut want);
        solver.apply(&ch.skeleton, &mut got);
        assert_eq!(got.local, want.local, "{:?}", set.constraints);
        compared += 1;
    }
    assert!(compared > 100 && rejected > 0, "{compared} sets compared, {rejected} refused: the sample proves little");
}
