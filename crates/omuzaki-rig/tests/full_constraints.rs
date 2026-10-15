//! The node-constraint stage inside `FullRig` (spec §27, EX-11): a constrained bone follows the IK result, a spring
//! chain under a constrained bone sees the constrained rotation, and a frame with constraints and springs allocates
//! nothing.
//!
//! The allocator counts only allocations made on the measuring thread, as in `full.rs`: the harness's own threads would
//! make a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{
    BoneConstraint, ConstraintKind, ConstraintSet, HumanoidBone, HumanoidPose, IkGoals, Limb, Pose, Rig, Skeleton, Tick,
};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_rig::constraint::VrmConstraintSolver;
use omuzaki_rig::full::FullRig;
use omuzaki_rig::skeleton::RigSkeleton;

#[path = "support/spring_rig.rs"]
mod spring_rig;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const FPS: f32 = 60.0;

/// A rotation constraint copying `source`'s rotation (as posed, after IK) onto `bone`, at full weight.
fn copy_rotation(skeleton: &Skeleton, bone: HumanoidBone, source: HumanoidBone) -> ConstraintSet {
    let bone = skeleton.humanoid_bone(bone).expect("a humanoid bone");
    let source = skeleton.humanoid_bone(source).expect("a humanoid bone");
    ConstraintSet { constraints: vec![BoneConstraint { bone, source, kind: ConstraintKind::Rotation, weight: 1.0 }] }
}

/// A pose that never rests: sways the spine and swings both arms, a function of the frame alone.
fn animated(frame: u64) -> HumanoidPose {
    let t = frame as f32;
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::Spine, Quat::from_rotation_z(0.2 * (0.05 * t).sin()));
    pose.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_x(0.6 * (0.07 * t).sin()));
    pose.set(HumanoidBone::RightUpperArm, Quat::from_rotation_x(0.6 * (0.06 * t).cos()));
    pose
}

/// The rest world position of `bone` on `skeleton`.
fn rest_at(skeleton: &Skeleton, bone: HumanoidBone) -> Vec3 {
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut world);
    world.local[skeleton.humanoid_bone(bone).expect("a required bone").index()].translation
}

/// Whether two poses agree to `eps` in every component of every bone (rotations compared as the same turn).
fn close(a: &Pose, b: &Pose, eps: f32) -> bool {
    a.local.iter().zip(&b.local).all(|(x, y)| {
        x.translation.abs_diff_eq(y.translation, eps)
            && (x.rotation.abs_diff_eq(y.rotation, eps) || x.rotation.abs_diff_eq(-y.rotation, eps))
    })
}

#[test]
fn a_constrained_bone_follows_the_ik_result_not_the_animated_pose() {
    let rig = spring_rig::hair_rig(1, 1);
    let skeleton = &rig.skeleton;
    let upper = rest_at(skeleton, HumanoidBone::LeftUpperArm);
    // A hand goal off the upper arm: IK turns the arm, and the hand's constraint copies that turn.
    let goals = IkGoals {
        left_hand: Some(Limb {
            target: upper + Vec3::new(0.12, 0.05, 0.1),
            pole: upper + Vec3::new(0.0, 0.0, 1.0),
            weight: 1.0,
        }),
        ..IkGoals::NONE
    };
    let solver = VrmConstraintSolver::new(
        &copy_rotation(skeleton, HumanoidBone::LeftHand, HumanoidBone::LeftUpperArm),
        skeleton,
    )
    .expect("the set is valid");
    let mut constrained = FullRig::without_springs(RigSkeleton::new(skeleton.clone())).with_constraints(solver);
    assert!(constrained.constraints().is_some());
    let mut out = Pose::IDENTITY;
    let pose = HumanoidPose::REST;

    constrained.set_goals(&goals);
    constrained.solve(Tick::at_rate(0, FPS), &pose, &mut out);
    let arm = skeleton.humanoid_bone(HumanoidBone::LeftUpperArm).expect("required").index();
    let hand = skeleton.humanoid_bone(HumanoidBone::LeftHand).expect("required").index();
    let posed = out.local[arm].rotation;
    assert!(posed.angle_between(Quat::IDENTITY) > 0.05, "IK did not turn the upper arm");
    assert!(out.local[hand].rotation.abs_diff_eq(posed, 1e-5), "the hand did not follow the IK-turned upper arm");

    // Without the goal the same constraint copies the animated (rest) arm, so the hand stays at rest.
    constrained.set_goals(&IkGoals::NONE);
    constrained.solve(Tick::at_rate(1, FPS), &pose, &mut out);
    assert!(out.local[hand].rotation.angle_between(Quat::IDENTITY) < 1e-5, "the hand followed the animated arm");
}

#[test]
fn a_spring_chain_under_a_constrained_bone_sees_the_constrained_rotation() {
    let rig = spring_rig::hair_rig(4, 5);
    let skeleton = &rig.skeleton;
    let rig_skeleton = || RigSkeleton::new(skeleton.clone());
    // The head copies the spine's rotation, and the hair hangs from the head. Its twin has no constraint and its head
    // posed at the spine's rotation: the hair must match the twin's bit for the tolerance of the solver, and differ
    // from the rig without constraints.
    let solver = VrmConstraintSolver::new(&copy_rotation(skeleton, HumanoidBone::Head, HumanoidBone::Spine), skeleton)
        .expect("the set is valid");
    let mut constrained = FullRig::new(rig_skeleton(), &rig.set).expect("valid springs").with_constraints(solver);
    let mut twin = FullRig::new(rig_skeleton(), &rig.set).expect("valid springs");
    let mut plain = FullRig::new(rig_skeleton(), &rig.set).expect("valid springs");
    let (mut a, mut b, mut c) = (Pose::IDENTITY, Pose::IDENTITY, Pose::IDENTITY);
    for frame in 0..120 {
        let tick = Tick::at_rate(frame, FPS);
        let mut posed = animated(frame);
        posed.set(HumanoidBone::Head, animated(frame).get(HumanoidBone::Spine));
        constrained.solve(tick, &animated(frame), &mut a);
        twin.solve(tick, &posed, &mut b);
        plain.solve(tick, &animated(frame), &mut c);
        assert!(close(&a, &b, 1e-4), "frame {frame}: the hair did not see the constrained head");
    }
    assert!(!close(&a, &c, 1e-3), "the constraint changed nothing");
}

#[test]
fn a_frame_with_constraints_and_springs_allocates_nothing() {
    // Everything that allocates is built before the counter starts: the hair, the constraints, and the goals.
    let rig = spring_rig::hair_rig(9, 8);
    let skeleton = &rig.skeleton;
    let mut set = copy_rotation(skeleton, HumanoidBone::Head, HumanoidBone::Spine);
    set.constraints.extend(copy_rotation(skeleton, HumanoidBone::LeftHand, HumanoidBone::LeftUpperArm).constraints);
    let solver = VrmConstraintSolver::new(&set, skeleton).expect("the set is valid");
    let mut full =
        FullRig::new(RigSkeleton::new(skeleton.clone()), &rig.set).expect("valid springs").with_constraints(solver);
    let goals: Vec<IkGoals> = (0..16)
        .map(|f| IkGoals {
            left_hand: Some(Limb {
                target: rest_at(skeleton, HumanoidBone::LeftUpperArm) + Vec3::new(0.1, 0.02 * f as f32, 0.1),
                pole: Vec3::Z,
                weight: 1.0,
            }),
            ..IkGoals::NONE
        })
        .collect();
    let poses: Vec<HumanoidPose> = (0..16).map(animated).collect();
    let mut out = Pose::IDENTITY;

    let mut frame = |i: u64| {
        let k = usize::try_from(i % 16).unwrap();
        full.set_goals(&goals[k]);
        full.solve(Tick::at_rate(i, FPS), &poses[k], &mut out);
    };
    frame(0); // warm-up: lazily sized state exists

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 1..=1000 {
        frame(i);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "1000 frames with constraints made {allocations} allocations");
}
