//! The full rig (`omuzaki_rig::full`): the stages run in the order spec §8 gives, a rig without goals or springs is the
//! humanoid solve, and a frame allocates nothing and repeats bit for bit.
//!
//! The allocator counts only allocations made on the measuring thread: the test harness's own threads allocate at
//! unpredictable times, which makes a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used, clippy::float_cmp, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{
    BoneDef, BoneId, HumanoidBone, HumanoidPose, IkGoals, IkSolver, Limb, LookAt, Pose, Rig, Skeleton, SpringChain,
    SpringParams, SpringSet, Tick, Transform,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::full::FullRig;
use omuzaki_rig::ik::TwoBoneIk;
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::spring::VrmSpringSolver;

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

/// The required humanoid bones with every bone 0.1 m above its parent (the hips at 1 m), plus `count` bones hanging
/// 0.1 m below `parent`, each the child of the one before. Returns the skeleton and the new bones.
fn with_chain(parent: HumanoidBone, count: usize) -> (Skeleton, Vec<BoneId>) {
    let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
    let base = Skeleton::humanoid(&required, |b| {
        Transform::from_translation(if b == HumanoidBone::Hips { Vec3::Y } else { Vec3::Y * 0.1 })
    })
    .expect("the required bones form a skeleton");
    let mut bones = base.bones().to_vec();
    let mut ids = Vec::new();
    let mut at = base.humanoid_bone(parent).expect("the parent is required");
    for i in 0..count {
        let id = BoneId(u16::try_from(bones.len()).expect("a few bones"));
        bones.push(BoneDef {
            name: format!("tail_{i}"),
            parent: Some(at),
            rest: Transform::from_translation(Vec3::NEG_Y * 0.1),
            humanoid: None,
        });
        ids.push(id);
        at = id;
    }
    (Skeleton::new(bones).expect("the chain follows its parent"), ids)
}

/// A chain that hangs under gravity: no pull back to its rest direction.
fn hanging(ids: &[BoneId]) -> SpringSet {
    let params = SpringParams { stiffness: 0.0, gravity_power: 1.0, drag_force: 0.4, ..SpringParams::default() };
    SpringSet { chains: vec![SpringChain::tapered("tail", ids, params, params)], groups: Vec::new() }
}

fn world(skeleton: &Skeleton, local: &Pose) -> Pose {
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(local, &mut world);
    world
}

fn at(skeleton: &Skeleton, world: &Pose, bone: HumanoidBone) -> Vec3 {
    world.local[skeleton.humanoid_bone(bone).expect("a required bone").index()].translation
}

/// A pose that never rests: sways the spine and head and swings both arms, a function of the frame alone.
fn animated(frame: u64) -> HumanoidPose {
    let t = frame as f32;
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::Spine, Quat::from_rotation_z(0.2 * (0.05 * t).sin()));
    pose.set(
        HumanoidBone::Head,
        Quat::from_euler(omuzaki_core::glam::EulerRot::XYZ, 0.3 * (0.13 * t).sin(), 0.5 * (0.09 * t).sin(), 0.0),
    );
    pose.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_x(0.6 * (0.07 * t).sin()));
    pose.set(HumanoidBone::RightUpperArm, Quat::from_rotation_x(0.6 * (0.06 * t).cos()));
    pose.hips_offset = Vec3::new(0.2 * (0.07 * t).sin(), 0.0, 0.1 * (0.05 * t).cos());
    pose
}

/// Goals that move every frame: both hands and feet reach about, and the head is looked at a point.
fn moving_goals(skeleton: &Skeleton, frame: u64) -> IkGoals {
    let t = frame as f32;
    let limb = |bone: HumanoidBone, phase: f32| {
        let root = bone_root(skeleton, bone);
        Limb {
            target: root
                + Vec3::new(0.12 * (0.1 * t + phase).sin(), 0.05 * (0.07 * t).cos(), 0.1 + 0.05 * (0.09 * t).sin()),
            pole: root + Vec3::new(0.0, 0.0, 1.0),
            weight: 0.5 + 0.5 * (0.03 * t + phase).sin(),
        }
    };
    IkGoals {
        look_at: Some(LookAt { target: Vec3::new(0.0, 1.5, 2.0), weight: 1.0, eye_share: 0.5 }),
        left_hand: Some(limb(HumanoidBone::LeftHand, 0.0)),
        right_hand: Some(limb(HumanoidBone::RightHand, 1.0)),
        left_foot: Some(limb(HumanoidBone::LeftFoot, 2.0)),
        right_foot: Some(limb(HumanoidBone::RightFoot, 3.0)),
    }
}

/// The rest world position of the limb root above `tip` (upper arm for a hand, upper leg for a foot).
fn bone_root(skeleton: &Skeleton, tip: HumanoidBone) -> Vec3 {
    let root = match tip {
        HumanoidBone::LeftHand => HumanoidBone::LeftUpperArm,
        HumanoidBone::RightHand => HumanoidBone::RightUpperArm,
        HumanoidBone::LeftFoot => HumanoidBone::LeftUpperLeg,
        _ => HumanoidBone::RightUpperLeg,
    };
    at(skeleton, &world(skeleton, &skeleton.rest_pose()), root)
}

#[test]
fn without_goals_or_springs_it_is_the_humanoid_rig_bit_for_bit() {
    let (skeleton, _) = with_chain(HumanoidBone::Head, 3);
    let rig_skeleton = RigSkeleton::new(skeleton);
    let mut humanoid = HumanoidRig::new(rig_skeleton.clone());
    let mut full = FullRig::without_springs(rig_skeleton);
    let (mut want, mut got) = (Pose::IDENTITY, Pose::IDENTITY);
    for frame in 0..120 {
        let pose = animated(frame);
        humanoid.solve(Tick::at_rate(frame, FPS), &pose, &mut want);
        full.solve(Tick::at_rate(frame, FPS), &pose, &mut got);
        assert!(want == got, "frame {frame} differs");
    }
}

#[test]
fn idle_goals_leave_the_humanoid_pose_alone() {
    let (skeleton, _) = with_chain(HumanoidBone::Head, 3);
    let rig_skeleton = RigSkeleton::new(skeleton);
    let mut humanoid = HumanoidRig::new(rig_skeleton.clone());
    let mut full = FullRig::without_springs(rig_skeleton);
    let limb = Limb { target: Vec3::ONE, pole: Vec3::Z, weight: 0.0 };
    full.set_goals(&IkGoals { left_hand: Some(limb), ..IkGoals::NONE });
    let (mut want, mut got) = (Pose::IDENTITY, Pose::IDENTITY);
    let pose = animated(9);
    humanoid.solve(Tick::at_rate(0, FPS), &pose, &mut want);
    full.solve(Tick::at_rate(0, FPS), &pose, &mut got);
    assert_eq!(want, got);
}

#[test]
fn springs_on_a_hand_follow_the_ik_result_not_the_animated_pose() {
    let (skeleton, ids) = with_chain(HumanoidBone::RightHand, 4);
    let upper = bone_root(&skeleton, HumanoidBone::RightHand);
    let target = upper + Vec3::new(0.12, 0.05, 0.1);
    let goals = IkGoals {
        right_hand: Some(Limb { target, pole: upper + Vec3::new(0.0, 0.0, 1.0), weight: 1.0 }),
        ..IkGoals::NONE
    };
    let settle = |goals: &IkGoals| {
        let mut rig = FullRig::new(RigSkeleton::new(skeleton.clone()), &hanging(&ids)).expect("valid springs");
        rig.set_goals(goals);
        let mut out = Pose::IDENTITY;
        for frame in 0..600 {
            rig.solve(Tick::at_rate(frame, FPS), &HumanoidPose::REST, &mut out);
        }
        world(&skeleton, &out)
    };
    let (with_ik, without) = (settle(&goals), settle(&IkGoals::NONE));

    let hand = at(&skeleton, &with_ik, HumanoidBone::RightHand);
    assert!(hand.distance(target) < 0.01, "IK brought the hand to {hand:?}, wanted {target:?}");
    assert!(hand.distance(at(&skeleton, &without, HumanoidBone::RightHand)) > 0.05, "the goal moved the hand");

    // Gravity hangs the chain straight down from its first joint, which rides on the IK hand: x and z of every later
    // joint stay on the first one's, and the first one is where the hand carries it.
    let anchor = |w: &Pose| w.local[ids[0].index()].translation;
    let (pinned, free) = (anchor(&with_ik), anchor(&without));
    assert!(pinned.distance(hand) < 0.11, "the chain root is not on the IK hand");
    for &id in &ids[1..] {
        let p = with_ik.local[id.index()].translation;
        assert!(
            (p.x - pinned.x).abs() < 0.02 && (p.z - pinned.z).abs() < 0.02,
            "tail {id:?} at {p:?} does not hang from {pinned:?}"
        );
        assert!(p.y < pinned.y, "tail {id:?} is not below its root");
    }
    // Without the goal it hangs from the animated hand, which is somewhere else.
    let tip = without.local[ids[3].index()].translation;
    assert!((tip.x - free.x).abs() < 0.02 && pinned.distance(free) > 0.05, "the goal moved the chain");
}

/// Whether two poses agree to `eps` in every component of every bone (rotations compared as the same turn).
fn close(a: &Pose, b: &Pose, eps: f32) -> bool {
    a.local.iter().zip(&b.local).all(|(x, y)| {
        x.translation.abs_diff_eq(y.translation, eps)
            && (x.rotation.abs_diff_eq(y.rotation, eps) || x.rotation.abs_diff_eq(-y.rotation, eps))
    })
}

/// Turns the head about Y by `weight` radians when a look-at goal is set: a stand-in look-at solver, so the order of the
/// stages can be tested before the real one lands.
struct HeadTurn;

impl IkSolver for HeadTurn {
    fn solve(&mut self, skeleton: &Skeleton, goals: &IkGoals, pose: &mut Pose) {
        if let (Some(look), Some(head)) = (goals.look_at, skeleton.humanoid_bone(HumanoidBone::Head)) {
            let turn = &mut pose.local[head.index()].rotation;
            *turn = Quat::from_rotation_y(look.weight) * *turn;
        }
    }

    fn reset(&mut self) {}
}

#[test]
fn look_at_turns_the_head_before_the_springs_on_the_head_swing() {
    let (skeleton, ids) = with_chain(HumanoidBone::Head, 4);
    let springs = SpringSet {
        chains: vec![SpringChain::tapered("hair", &ids, SpringParams::default(), SpringParams::default())],
        groups: Vec::new(),
    };
    let build = || {
        let solver = VrmSpringSolver::new(&springs, &skeleton).expect("valid springs");
        FullRig::from_parts(RigSkeleton::new(skeleton.clone()), HeadTurn, Some(solver))
    };
    // Turning the head by the goal must equal turning it in the animation: the springs saw the turned head either way.
    let (mut by_goal, mut by_pose, mut plain) = (build(), build(), build());
    by_goal.set_goals(&IkGoals {
        look_at: Some(LookAt { target: Vec3::Z, weight: 0.8, eye_share: 0.0 }),
        ..IkGoals::NONE
    });
    let (mut a, mut b, mut c) = (Pose::IDENTITY, Pose::IDENTITY, Pose::IDENTITY);
    for frame in 0..120 {
        let tick = Tick::at_rate(frame, FPS);
        let mut turned = animated(frame);
        turned.set(HumanoidBone::Head, Quat::from_rotation_y(0.8) * turned.get(HumanoidBone::Head));
        by_goal.solve(tick, &animated(frame), &mut a);
        by_pose.solve(tick, &turned, &mut b);
        plain.solve(tick, &animated(frame), &mut c);
        // Not bit for bit: the pose path normalises its quaternion and the solver's multiply does not.
        assert!(close(&a, &b, 1e-4), "frame {frame}: the springs did not see the look-at turn");
    }
    assert!(!close(&a, &c, 1e-3), "the look-at goal changed nothing");
}

#[test]
fn solve_and_skin_matches_a_separate_skinner() {
    let rig = spring_rig::hair_rig(4, 5);
    let rig_skeleton = RigSkeleton::new(rig.skeleton.clone());
    let mut full = FullRig::new(rig_skeleton.clone(), &rig.set).expect("valid springs");
    let mut reference = FullRig::new(rig_skeleton.clone(), &rig.set).expect("valid springs");
    let mut skinner = omuzaki_rig::skin::Skinner::new(rig_skeleton);
    let (mut out, mut want_local) = (Pose::IDENTITY, Pose::IDENTITY);
    let mut joints = vec![Mat4::ZERO; rig.skeleton.len()];
    let mut want = vec![Mat4::ZERO; rig.skeleton.len()];
    for frame in 0..60 {
        let tick = Tick::at_rate(frame, FPS);
        full.solve_and_skin(tick, &animated(frame), &mut out, &mut joints);
        reference.solve(tick, &animated(frame), &mut want_local);
        skinner.skin(&want_local, &mut want);
        assert!(out == want_local && joints == want, "frame {frame}");
        let n = rig.skeleton.len();
        assert_eq!(full.world().local[..n], world(&rig.skeleton, &out).local[..n]);
    }
}

#[test]
fn the_same_inputs_give_the_same_bits_over_600_ticks() {
    let rig = spring_rig::hair_rig(9, 8);
    let build = || FullRig::new(RigSkeleton::new(rig.skeleton.clone()), &rig.set).expect("valid springs");
    let (mut one, mut two) = (build(), build());
    let (mut a, mut b) = (Pose::IDENTITY, Pose::IDENTITY);
    let n = rig.skeleton.len();
    let (mut ja, mut jb) = (vec![Mat4::ZERO; n], vec![Mat4::ZERO; n]);
    for frame in 0..600 {
        let tick = Tick::at_rate(frame, FPS);
        let goals = moving_goals(&rig.skeleton, frame);
        one.set_goals(&goals);
        two.set_goals(&goals);
        one.solve_and_skin(tick, &animated(frame), &mut a, &mut ja);
        two.solve_and_skin(tick, &animated(frame), &mut b, &mut jb);
        assert!(a.local[..n] == b.local[..n], "frame {frame}: poses differ");
        let bits = |m: &[Mat4]| m.iter().flat_map(|m| m.to_cols_array().map(f32::to_bits)).collect::<Vec<_>>();
        assert!(bits(&ja) == bits(&jb), "frame {frame}: palettes differ");
    }
    // A reset repeats the run too.
    one.reset();
    let (mut c, mut jc) = (Pose::IDENTITY, vec![Mat4::ZERO; n]);
    let mut three = build();
    for frame in 0..30 {
        let tick = Tick::at_rate(frame, FPS);
        let goals = moving_goals(&rig.skeleton, frame);
        one.set_goals(&goals);
        three.set_goals(&goals);
        one.solve_and_skin(tick, &animated(frame), &mut a, &mut ja);
        three.solve_and_skin(tick, &animated(frame), &mut c, &mut jc);
        assert!(a.local[..n] == c.local[..n], "frame {frame}: a reset rig differs from a new one");
    }
}

#[test]
fn a_full_frame_allocates_nothing() {
    // Everything that allocates is built before the counter starts: the 30-chain hair, IK on all four limbs.
    let rig = spring_rig::hair_rig(9, 8);
    let n = rig.skeleton.len();
    let mut full: FullRig<TwoBoneIk, VrmSpringSolver> =
        FullRig::new(RigSkeleton::new(rig.skeleton.clone()), &rig.set).expect("valid springs");
    let goals: Vec<IkGoals> = (0..16).map(|f| moving_goals(&rig.skeleton, f)).collect();
    let poses: Vec<HumanoidPose> = (0..16).map(animated).collect();
    let mut out = Pose::IDENTITY;
    let mut scene = Scene::new(Camera::default());
    scene.joints.resize(n, Mat4::IDENTITY);

    let mut frame = |i: u64| {
        let k = usize::try_from(i % 16).unwrap();
        full.set_goals(&goals[k]);
        let tick = Tick::at_rate(i, [30.0, 60.0, 144.0][k % 3]);
        full.solve_and_skin(tick, &poses[k], &mut out, &mut scene.joints);
    };
    frame(0); // warm-up: lazily sized state exists

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 1..=1000 {
        frame(i);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "1000 full-rig frames made {allocations} allocations");
}

/// FNV-1a over the bit patterns of every number in `values`, the digest the rig's regression is pinned to.
fn fnv(values: impl Iterator<Item = u32>) -> u64 {
    values.fold(0xcbf2_9ce4_8422_2325_u64, |h, bits| (h ^ u64::from(bits)).wrapping_mul(0x0100_0000_01b3))
}

#[test]
fn the_regression_digest_of_solve_and_skin_is_pinned() {
    let rig = spring_rig::hair_rig(9, 8);
    let n = rig.skeleton.len();
    let mut full = FullRig::new(RigSkeleton::new(rig.skeleton.clone()), &rig.set).expect("valid springs");
    let (mut out, mut joints) = (Pose::IDENTITY, vec![Mat4::ZERO; n]);
    let (mut pose_bits, mut joint_bits) = (Vec::new(), Vec::new());
    for frame in 0..240 {
        full.set_goals(&moving_goals(&rig.skeleton, frame));
        full.solve_and_skin(Tick::at_rate(frame, FPS), &animated(frame), &mut out, &mut joints);
        for t in &out.local[..n] {
            pose_bits.extend(t.translation.to_array().into_iter().chain(t.rotation.to_array()).map(f32::to_bits));
        }
        joint_bits.extend(joints.iter().flat_map(|m| m.to_cols_array().map(f32::to_bits)));
    }
    // Pinned on the commit before the constraint stage existed: a rig without constraints must not move a bit.
    assert_eq!(fnv(pose_bits.into_iter()), 2_721_995_266_870_702_916, "the pose digest moved");
    assert_eq!(fnv(joint_bits.into_iter()), 5_968_876_091_311_377_936, "the palette digest moved");
}
