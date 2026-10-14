//! The two-bone IK solver against the issue's acceptance: reachable targets hit within 0.5 mm, unreachable ones give a
//! straight limb pointing at the target, a full sweep never flips the pole, weight 0 is the identity, hinge limits
//! hold, and a solve allocates nothing.
//!
//! Expectations are computed from world positions of the posed skeleton (forward kinematics), not from the solver's
//! own maths. Every test runs on the aligned humanoid and on the one whose bone axes (armature root included) are
//! tilted at random, since the solve must not depend on how a skeleton's local axes are laid out.
//!
//! It is its own test binary because the counting allocator is global; it counts only the measuring thread.

// Geometry tests name points a, b, c (root, mid, tip) and `pose`/`post` pairs the way the maths does.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::similar_names
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{
    BoneDef, BoneId, HumanoidBone as B, HumanoidPose, IkGoals, IkSolver, Limb, Pose, Skeleton, Transform,
};
use omuzaki_anim_mock::fixtures::quadruped_rig;
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_rig::ik::TwoBoneIk;
use omuzaki_rig::ik::two_bone::{MAX_FLEXION, NEAR_STRAIGHT, SOFT_REACH};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;

mod common;

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

/// The four limbs: goal slot, `[root, mid, tip]`, and the bend the module promises when the pole names none.
const LIMBS: [(usize, [B; 3], Vec3); 4] = [
    (0, [B::LeftUpperArm, B::LeftLowerArm, B::LeftHand], Vec3::new(1.0, -1.0, -2.0)),
    (1, [B::RightUpperArm, B::RightLowerArm, B::RightHand], Vec3::new(-1.0, -1.0, -2.0)),
    (2, [B::LeftUpperLeg, B::LeftLowerLeg, B::LeftFoot], Vec3::Z),
    (3, [B::RightUpperLeg, B::RightLowerLeg, B::RightFoot], Vec3::Z),
];

fn goals(slot: usize, limb: Limb) -> IkGoals {
    let mut g = IkGoals::NONE;
    match slot {
        0 => g.left_hand = Some(limb),
        1 => g.right_hand = Some(limb),
        2 => g.left_foot = Some(limb),
        _ => g.right_foot = Some(limb),
    }
    g
}

fn world(skeleton: &Skeleton, pose: &Pose) -> Pose {
    let mut out = Pose::IDENTITY;
    skeleton.world_pose(pose, &mut out);
    out
}

struct Fixture {
    ch: common::Character,
    rig: HumanoidRig,
}

impl Fixture {
    fn new(tilted: bool) -> Self {
        let ch = common::full_character(tilted);
        let rig = HumanoidRig::new(RigSkeleton::new(ch.skeleton.clone()));
        Self { ch, rig }
    }

    /// The animated starting poses: the rest pose, then ones the humanoid stage bent at random.
    fn starts(&self) -> Vec<Pose> {
        let mut rng = Rng::new(11);
        let mut out = vec![self.ch.skeleton.rest_pose()];
        for _ in 0..5 {
            let mut pose = Pose::IDENTITY;
            self.rig.apply_humanoid(&common::random_pose(&mut rng, 0.6), &mut pose);
            out.push(pose);
        }
        out
    }

    fn ids(&self, bones: [B; 3]) -> [BoneId; 3] {
        bones.map(|b| self.ch.skeleton.humanoid_bone(b).unwrap())
    }

    /// World positions of a limb's root, mid and tip.
    fn joints(&self, pose: &Pose, bones: [B; 3]) -> [Vec3; 3] {
        let w = world(&self.ch.skeleton, pose);
        self.ids(bones).map(|id| w.local[id.index()].translation)
    }
}

fn both() -> [Fixture; 2] {
    [Fixture::new(false), Fixture::new(true)]
}

/// Elbow or knee bend, radians from straight, from world joint positions. Taken from the cross product, which stays
/// accurate for a nearly straight limb where an `acos` of the dot product has a floor of about 4.5e-4 rad.
fn flexion([a, b, c]: [Vec3; 3]) -> f32 {
    let (upper, lower) = ((b - a).normalize(), (c - b).normalize());
    upper.cross(lower).length().atan2(upper.dot(lower))
}

fn limb(target: Vec3, pole: Vec3, weight: f32) -> Limb {
    Limb { target, pole, weight }
}

fn random_dir(rng: &mut Rng) -> Vec3 {
    loop {
        let v = Vec3::new(rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0));
        if v.length_squared() > 0.01 && v.length_squared() <= 1.0 {
            return v.normalize();
        }
    }
}

#[test]
fn reachable_targets_are_hit_within_half_a_millimetre_and_lengths_are_kept() {
    let mut rng = Rng::new(3);
    let mut worst = 0.0_f32;
    for fx in both() {
        for start in fx.starts() {
            for (slot, bones, _) in LIMBS {
                let [a, b, c] = fx.joints(&start, bones);
                let (l1, l2) = (a.distance(b), b.distance(c));
                // Between the fully bent hinge and the start of the soft zone, every direction.
                let bent = (l1 * l1 + l2 * l2 - 2.0 * l1 * l2 * (PI - MAX_FLEXION).cos()).sqrt();
                for _ in 0..40 {
                    let dist = rng.range_f32(bent * 1.02, (l1 + l2) * (1.0 - SOFT_REACH) * 0.999);
                    let target = a + random_dir(&mut rng) * dist;
                    let pole = a + random_dir(&mut rng);
                    let mut pose = start.clone();
                    TwoBoneIk.solve(&fx.ch.skeleton, &goals(slot, limb(target, pole, 1.0)), &mut pose);
                    let [a2, b2, c2] = fx.joints(&pose, bones);
                    worst = worst.max(c2.distance(target));
                    assert!(c2.distance(target) < 5e-4, "limb {slot}: tip {} m off", c2.distance(target));
                    assert!(a2.distance(a) < 1e-5, "the root joint does not move");
                    assert!((a2.distance(b2) - l1).abs() < 1e-5 && (b2.distance(c2) - l2).abs() < 1e-5);
                    // The bend faces the pole's side of the root-to-target line.
                    let line = (target - a).normalize();
                    let off = |p: Vec3| (p - a) - line * (p - a).dot(line);
                    assert!(off(b2).dot(off(pole)) >= -1e-6, "limb {slot}: elbow on the wrong side");
                }
            }
        }
    }
    eprintln!("worst tip error {worst:e} m");
}

#[test]
fn unreachable_targets_give_a_straight_limb_pointing_at_the_target() {
    let mut rng = Rng::new(5);
    for fx in both() {
        for start in fx.starts() {
            for (slot, bones, _) in LIMBS {
                let [a, b, c] = fx.joints(&start, bones);
                let reach = a.distance(b) + b.distance(c);
                for scale in [1.5, 3.0, 20.0] {
                    let dir = random_dir(&mut rng);
                    let target = a + dir * reach * scale;
                    let mut pose = start.clone();
                    TwoBoneIk.solve(
                        &fx.ch.skeleton,
                        &goals(slot, limb(target, a + random_dir(&mut rng), 1.0)),
                        &mut pose,
                    );
                    let joints = fx.joints(&pose, bones);
                    assert!(flexion(joints) < 1e-3, "limb {slot}: still bent {} rad at {scale}x", flexion(joints));
                    let tip = joints[2] - joints[0];
                    assert!((tip.length() - reach).abs() < 1e-4, "straight means full length");
                    assert!(tip.normalize().dot(dir) > 0.99999, "points at the target");
                }
            }
        }
    }
}

#[test]
fn the_last_two_percent_of_reach_eases_instead_of_snapping_straight() {
    let fx = Fixture::new(false);
    let start = fx.ch.skeleton.rest_pose();
    let (slot, bones, _) = LIMBS[1];
    let [a, b, c] = fx.joints(&start, bones);
    let reach = a.distance(b) + b.distance(c);
    // Walk the target out along -Z (bent elbow facing -Y) across the whole zone and beyond.
    let mut last = f32::INFINITY;
    let mut steps = 0;
    for i in 0..=140 {
        let dist = reach * (0.9 + 0.004 * i as f32);
        let mut pose = start.clone();
        let g = goals(slot, limb(a + Vec3::Z * dist, a - Vec3::Y, 1.0));
        TwoBoneIk.solve(&fx.ch.skeleton, &g, &mut pose);
        let f = flexion(fx.joints(&pose, bones));
        assert!(f <= last + 1e-6, "flexion only falls as the target recedes");
        assert!(last.is_infinite() || last - f < 0.12, "no snap: {last} -> {f} in one step");
        last = f;
        if dist > reach * 0.98 && dist < reach {
            steps += 1;
            assert!(f > 0.0, "inside the reach the limb is eased, not straight");
        }
    }
    assert!(steps > 3 && last < 1e-3);
}

#[test]
fn hinges_stay_between_straight_and_the_limit() {
    let mut rng = Rng::new(9);
    for fx in both() {
        for start in fx.starts() {
            for (slot, bones, _) in LIMBS {
                let [a, ..] = fx.joints(&start, bones);
                // Targets from inside the root joint (hard bend) to far out.
                for dist in [0.0, 0.001, 0.05, 0.2, 0.5, 2.0] {
                    let target = a + random_dir(&mut rng) * dist;
                    let mut pose = start.clone();
                    TwoBoneIk.solve(
                        &fx.ch.skeleton,
                        &goals(slot, limb(target, a + random_dir(&mut rng), 1.0)),
                        &mut pose,
                    );
                    let f = flexion(fx.joints(&pose, bones));
                    assert!((0.0..=MAX_FLEXION + 1e-3).contains(&f), "limb {slot} at {dist} m: flexion {f}");
                    if dist < 0.05 {
                        assert!(f > MAX_FLEXION - 1e-3, "a target inside the limit bends the hinge to it");
                    }
                }
            }
        }
    }
}

#[test]
fn a_full_sweep_of_the_target_never_flips_the_pole() {
    for fx in both() {
        for (slot, bones, _) in LIMBS {
            let start = fx.ch.skeleton.rest_pose();
            let [a, b, c] = fx.joints(&start, bones);
            let dist = 0.7 * (a.distance(b) + b.distance(c));
            // The pole is fixed on +Z; the target circles the root in the XY plane, so the line to it is never along
            // the pole and the bend must stay on +Z for the whole turn, moving only a little between frames.
            let pole = a + Vec3::Z;
            let mut last: Option<Vec3> = None;
            let n = 720;
            for i in 0..=n {
                let angle = TAU * i as f32 / n as f32;
                let target = a + Vec3::new(angle.cos(), angle.sin(), 0.0) * dist;
                let mut pose = start.clone();
                TwoBoneIk.solve(&fx.ch.skeleton, &goals(slot, limb(target, pole, 1.0)), &mut pose);
                let [_, mid, tip] = fx.joints(&pose, bones);
                assert!(tip.distance(target) < 5e-4);
                let bend = (mid - a).normalize();
                assert!((mid - a).dot(Vec3::Z) > 0.0, "limb {slot} flipped at {angle} rad");
                if let Some(prev) = last {
                    assert!(bend.angle_between(prev) < 0.03, "limb {slot}: elbow jumped at step {i}");
                }
                last = Some(bend);
            }
        }
    }
}

#[test]
fn the_bend_follows_the_pole_all_the_way_round() {
    let fx = Fixture::new(true);
    let start = fx.ch.skeleton.rest_pose();
    for (slot, bones, _) in LIMBS {
        let [a, b, c] = fx.joints(&start, bones);
        let target = a + Vec3::new(0.2, -0.3, 0.25).normalize() * 0.7 * (a.distance(b) + b.distance(c));
        let axis = (target - a).normalize();
        let (u, v) = (axis.any_orthonormal_pair().0, axis.any_orthonormal_pair().1);
        for i in 0..72 {
            let angle = TAU * i as f32 / 72.0;
            let want = u * angle.cos() + v * angle.sin();
            let mut pose = start.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &goals(slot, limb(target, a + want * 2.0, 1.0)), &mut pose);
            let [_, mid, _] = fx.joints(&pose, bones);
            let off = (mid - a) - axis * (mid - a).dot(axis);
            assert!(off.normalize().dot(want) > 0.9999, "limb {slot}: bend off the pole at {angle} rad");
        }
    }
}

#[test]
fn a_pole_on_the_line_keeps_the_animated_side_and_then_the_default() {
    for fx in both() {
        // Animated side: a bent arm, target straight ahead of the shoulder, pole on that line.
        let mut pose = Pose::IDENTITY;
        let mut bend = HumanoidPose::REST;
        bend.set(B::RightLowerArm, Quat::from_rotation_y(1.0));
        fx.rig.apply_humanoid(&bend, &mut pose);
        let bones = LIMBS[1].1;
        let [a, b, c] = fx.joints(&pose, bones);
        let target = a + Vec3::Z * 0.8 * (a.distance(b) + b.distance(c));
        let before = (b - a) - Vec3::Z * (b - a).dot(Vec3::Z);
        let mut solved = pose.clone();
        TwoBoneIk.solve(&fx.ch.skeleton, &goals(1, limb(target, a + Vec3::Z, 1.0)), &mut solved);
        let [_, mid, _] = fx.joints(&solved, bones);
        let after = (mid - a) - Vec3::Z * (mid - a).dot(Vec3::Z);
        assert!(after.normalize().dot(before.normalize()) > 0.999, "keeps the animated side");

        // Default side: a straight limb, target on the line inside the reach, pole at the root. Elbows go back and
        // out, knees forward.
        let rest = fx.ch.skeleton.rest_pose();
        for (slot, bones, expect) in LIMBS {
            let [a, b, c] = fx.joints(&rest, bones);
            let target = a + (b - a).normalize() * 0.8 * (a.distance(b) + b.distance(c));
            let mut solved = rest.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &goals(slot, limb(target, a, 1.0)), &mut solved);
            let [_, mid, _] = fx.joints(&solved, bones);
            let axis = (target - a).normalize();
            let off = (mid - a) - axis * (mid - a).dot(axis);
            let expect = expect - axis * expect.dot(axis);
            assert!(off.normalize().dot(expect.normalize()) > 0.999, "limb {slot}: default bend");
        }
    }
}

#[test]
fn weight_zero_is_the_identity_and_weight_moves_the_tip_toward_the_goal() {
    let fx = Fixture::new(true);
    for start in fx.starts() {
        for (slot, bones, _) in LIMBS {
            let [a, b, c] = fx.joints(&start, bones);
            let target = a + Vec3::new(0.1, -0.2, 0.3).normalize() * 0.6 * (a.distance(b) + b.distance(c));
            let g = |w: f32| goals(slot, limb(target, a + Vec3::new(0.3, 0.1, -1.0), w));
            let mut pose = start.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &g(0.0), &mut pose);
            assert!(pose == start, "limb {slot}: weight 0 changed the pose");
            let mut pose = start.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &g(-1.0), &mut pose);
            assert!(pose == start, "a negative weight is zero");

            // A hair of weight is a hair of change: the tip stays within a millimetre of the animated pose, and so does
            // the elbow of a bent limb. (A straight limb shortened by d swings its elbow out by about sqrt(2·l·d), so
            // there the elbow moves millimetres for a micron of reach: geometry, not a jump.)
            let mut hair = start.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &g(1e-4), &mut hair);
            let (before, after) = (fx.joints(&start, bones), fx.joints(&hair, bones));
            assert!(before[2].distance(after[2]) < 1e-3, "limb {slot}: the tip jumped at weight 1e-4");
            if flexion(before) > 0.2 {
                assert!(before[1].distance(after[1]) < 1e-3, "limb {slot}: the elbow jumped at weight 1e-4");
            }

            let mut last = c.distance(target);
            for w in [0.25, 0.5, 0.75, 1.0] {
                let mut pose = start.clone();
                TwoBoneIk.solve(&fx.ch.skeleton, &g(w), &mut pose);
                let err = fx.joints(&pose, bones)[2].distance(target);
                assert!(err <= last + 1e-5, "limb {slot}: weight {w} is farther ({err}) than the one before ({last})");
                last = err;
            }
            assert!(last < 5e-4);
            let mut pose = start.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &g(7.0), &mut pose);
            assert!(fx.joints(&pose, bones)[2].distance(target) < 5e-4, "weights above one are one");
        }
    }
}

#[test]
fn only_the_two_chain_rotations_change() {
    for fx in both() {
        let start = fx.starts().remove(2);
        let [root, mid, _] = fx.ids(LIMBS[0].1);
        let [a, ..] = fx.joints(&start, LIMBS[0].1);
        let mut pose = start.clone();
        TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(a + Vec3::new(0.2, -0.3, 0.3), a - Vec3::Z, 1.0)), &mut pose);
        for i in 0..fx.ch.skeleton.len() {
            assert_eq!(pose.local[i].translation, start.local[i].translation, "translation of bone {i}");
            let chain = i == root.index() || i == mid.index();
            assert_eq!(pose.local[i].rotation != start.local[i].rotation, chain, "rotation of bone {i}");
        }
    }
}

#[test]
fn bad_goals_and_unsuitable_skeletons_change_nothing() {
    let fx = Fixture::new(false);
    let start = fx.starts().remove(1);
    let [a, ..] = fx.joints(&start, LIMBS[0].1);
    for bad in [
        limb(Vec3::new(f32::NAN, 0.0, 0.0), a, 1.0),
        limb(a + Vec3::X, Vec3::new(0.0, f32::INFINITY, 0.0), 1.0),
        limb(a + Vec3::X, a, f32::NAN),
    ] {
        let mut pose = start.clone();
        TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, bad), &mut pose);
        assert!(pose == start, "a non-finite goal is ignored");
    }

    let quad = quadruped_rig().unwrap();
    let rest = quad.skeleton.rest_pose();
    let mut pose = rest.clone();
    TwoBoneIk.solve(&quad.skeleton, &goals(0, limb(Vec3::ONE, Vec3::Z, 1.0)), &mut pose);
    assert!(pose == rest, "a generic skeleton is left alone");

    // A humanoid without arms (the 15 required bones) skips the limbs it lacks and still solves the legs.
    let required: Vec<B> = B::ALL.into_iter().filter(|b| b.is_required()).collect();
    let small = common::character(&required, false, 0, 0, 1);
    let rest = small.skeleton.rest_pose();
    let mut pose = rest.clone();
    TwoBoneIk.solve(&small.skeleton, &goals(0, limb(Vec3::new(0.3, 1.2, 0.3), Vec3::Z, 1.0)), &mut pose);
    assert!(pose == rest || small.skeleton.humanoid_bone(B::LeftHand).is_some());
}

#[test]
fn the_same_inputs_give_the_same_pose_bit_for_bit_and_reset_changes_nothing() {
    let fx = Fixture::new(true);
    let start = fx.starts().remove(3);
    let [a, ..] = fx.joints(&start, LIMBS[2].1);
    let g = IkGoals {
        left_foot: Some(limb(a + Vec3::new(0.1, -0.5, 0.3), a + Vec3::Z, 0.8)),
        right_hand: Some(limb(a + Vec3::new(-0.4, 0.5, 0.3), a - Vec3::Z, 0.6)),
        ..IkGoals::NONE
    };
    let mut ik = TwoBoneIk::new();
    let (mut first, mut second) = (start.clone(), start.clone());
    ik.solve(&fx.ch.skeleton, &g, &mut first);
    ik.reset();
    ik.solve(&fx.ch.skeleton, &g, &mut second);
    assert_eq!(first, second);
    assert_ne!(first, start);
}

#[test]
fn a_solve_allocates_nothing() {
    let fx = Fixture::new(true);
    let start = fx.starts().remove(2);
    let [a, ..] = fx.joints(&start, LIMBS[0].1);
    let mut ik = TwoBoneIk::new();
    let mut pose = start.clone();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 0..2000 {
        let t = i as f32 * 0.01;
        let g = IkGoals {
            left_hand: Some(limb(a + Vec3::new(t.sin(), t.cos(), 0.3) * 0.5, a - Vec3::Z, 1.0)),
            right_hand: Some(limb(a + Vec3::new(-t.cos(), t.sin(), 0.4) * 0.5, a - Vec3::Z, 0.5)),
            left_foot: Some(limb(Vec3::new(0.1, 0.1 + 0.2 * t.sin().abs(), 0.2), Vec3::Z, 1.0)),
            right_foot: Some(limb(Vec3::new(-0.1, 0.0, 5.0), Vec3::Z, 1.0)),
            ..IkGoals::NONE
        };
        ik.solve(&fx.ch.skeleton, &g, &mut pose);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "2000 four-limb solves made {allocations} allocations");
}

/// A pose with the left arm swung down 70 degrees and the elbow bent 140: inside the hinge limit.
fn bent_left_arm(fx: &Fixture) -> Pose {
    let mut humanoid = HumanoidPose::REST;
    humanoid.set(B::LeftUpperArm, Quat::from_rotation_z(-70.0_f32.to_radians()));
    humanoid.set(B::LeftLowerArm, Quat::from_rotation_z(140.0_f32.to_radians()));
    let mut pose = Pose::IDENTITY;
    fx.rig.apply_humanoid(&humanoid, &mut pose);
    pose
}

#[test]
fn a_partial_weight_never_folds_the_hinge_past_its_limit() {
    // The review's case: target at the current hand, pole above the shoulder, so the bend sweeps half way round while
    // the elbow is already at 140 degrees. Blending the two joint rotations folded it to 180.
    for fx in both() {
        let start = bent_left_arm(&fx);
        let bones = LIMBS[0].1;
        let [a, b, c] = fx.joints(&start, bones);
        assert!(flexion([a, b, c]) < MAX_FLEXION, "the fixture pose is inside the limit");
        for w in [0.05, 0.25, 0.5, 0.75, 0.95, 1.0] {
            let mut pose = start.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(c, a + Vec3::Y, w)), &mut pose);
            let joints = fx.joints(&pose, bones);
            assert!(flexion(joints) <= MAX_FLEXION + 1e-3, "weight {w}: flexion {}", flexion(joints));
            assert!(joints[2].distance(c) < 1e-3, "weight {w}: the hand moved {} m", joints[2].distance(c));
        }
    }

    // And over random animated poses, goals and weights: a hinge that starts inside its limit stays inside.
    let mut rng = Rng::new(21);
    for fx in both() {
        for start in fx.starts() {
            for (slot, bones, _) in LIMBS {
                let [a, b, c] = fx.joints(&start, bones);
                let allowed = MAX_FLEXION.max(flexion([a, b, c])) + 1e-3;
                for _ in 0..30 {
                    let target = a + random_dir(&mut rng) * rng.range_f32(0.0, 1.2);
                    let g = goals(slot, limb(target, a + random_dir(&mut rng), rng.range_f32(0.0, 1.0)));
                    let mut pose = start.clone();
                    TwoBoneIk.solve(&fx.ch.skeleton, &g, &mut pose);
                    let f = flexion(fx.joints(&pose, bones));
                    assert!(f <= allowed, "limb {slot}: flexion {f} past {allowed}");
                }
            }
        }
    }
}

/// `skeleton` with a twist bone between every upper and lower limb bone, tilted at rest, as a rig with twist helpers
/// has (the humanoid hierarchy permits non-humanoid bones between joints).
fn with_twist_bones(skeleton: &Skeleton) -> Skeleton {
    let lowers = [B::LeftLowerArm, B::RightLowerArm, B::LeftLowerLeg, B::RightLowerLeg];
    let mut bones: Vec<BoneDef> = Vec::new();
    let mut remap = vec![0_u16; skeleton.len()];
    for (i, def) in skeleton.bones().iter().enumerate() {
        let mut def = def.clone();
        def.parent = def.parent.map(|p| BoneId(remap[p.index()]));
        if def.humanoid.is_some_and(|h| lowers.contains(&h)) {
            let mut twist = def.clone();
            twist.name = format!("{}_twist", def.name);
            twist.humanoid = None;
            twist.rest.translation = def.rest.translation * 0.5;
            twist.rest.rotation = Quat::from_rotation_y(0.4) * def.rest.rotation;
            def.parent = Some(BoneId(u16::try_from(bones.len()).unwrap()));
            def.rest = Transform { translation: twist.rest.translation, rotation: Quat::from_rotation_y(0.2) };
            bones.push(twist);
        }
        remap[i] = u16::try_from(bones.len()).unwrap();
        bones.push(def);
    }
    Skeleton::new(bones).unwrap()
}

#[test]
fn helper_bones_between_the_joints_are_carried_along_and_the_limb_still_solves() {
    let base = Fixture::new(true);
    let skeleton = with_twist_bones(&base.ch.skeleton);
    let rig = HumanoidRig::new(RigSkeleton::new(skeleton.clone()));
    let mut rng = Rng::new(31);
    let mut hits = 0;
    for _ in 0..8 {
        let mut start = Pose::IDENTITY;
        rig.apply_humanoid(&common::random_pose(&mut rng, 0.6), &mut start);
        for (slot, bones, _) in LIMBS {
            let ids = bones.map(|b| skeleton.humanoid_bone(b).unwrap());
            assert_ne!(skeleton.bones()[ids[1].index()].parent, Some(ids[0]), "a twist bone sits between");
            let joints = |pose: &Pose| {
                let w = world(&skeleton, pose);
                ids.map(|id| w.local[id.index()].translation)
            };
            let [a, b, c] = joints(&start);
            let (l1, l2) = (a.distance(b), b.distance(c));
            let target = a + random_dir(&mut rng) * 0.8 * (l1 + l2);
            let mut pose = start.clone();
            TwoBoneIk.solve(&skeleton, &goals(slot, limb(target, a + random_dir(&mut rng), 1.0)), &mut pose);
            let [a2, b2, c2] = joints(&pose);
            assert!(c2.distance(target) < 5e-4, "limb {slot}: tip {} m off", c2.distance(target));
            assert!(
                a2.distance(a) < 1e-5 && (a2.distance(b2) - l1).abs() < 1e-5 && (b2.distance(c2) - l2).abs() < 1e-5
            );
            assert!(flexion([a2, b2, c2]) <= MAX_FLEXION + 1e-3);
            hits += 1;
        }
    }
    assert_eq!(hits, 32);
}

/// Elbow moves between consecutive samples, largest first.
fn jumps(points: &[Vec3]) -> Vec<f32> {
    let mut steps: Vec<f32> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
    steps.sort_by(|a, b| b.total_cmp(a));
    steps
}

#[test]
fn a_partial_weight_target_sweep_is_smooth_except_across_the_documented_opposite() {
    // At a fixed partial weight a target crossing the exact opposite of the animated aim or bend makes the elbow jump
    // to the other way round (see the module docs: no blend avoids it). Everywhere else it moves smoothly, and the jump
    // itself is bounded by the limb's size.
    for fx in both() {
        let start = bent_left_arm(&fx);
        let bones = LIMBS[0].1;
        let [a, b, c] = fx.joints(&start, bones);
        let upper = a.distance(b);
        let elbow_at = |target: Vec3, pole: Vec3, w: f32| {
            let mut pose = start.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
            fx.joints(&pose, bones)[1]
        };
        for w in [0.1, 0.25, 0.5, 0.75, 0.9] {
            // Within a few millimetres of the animated hand, pole above the shoulder: the solved bend side is opposite
            // the animated one, and flips sides at the hand.
            let line: Vec<Vec3> =
                (0..=100).map(|i| elbow_at(c + Vec3::Z * (-0.005 + 0.0001 * i as f32), a + Vec3::Y, w)).collect();
            let steps = jumps(&line);
            assert!(steps[1] < 0.01, "weight {w}: a second jump of {} m near the hand", steps[1]);
            eprintln!("weight {w}: crossing jump near the hand {:.4} m", steps[0]);
            // The elbow turns half way round the limb's axis by `w`, so it moves at most a chord 2·l·sin(π·w).
            let chord = 2.0 * upper * (PI * w).sin() + 0.02;
            assert!(steps[0] <= chord, "weight {w}: the crossing jump {} m is past the chord {chord}", steps[0]);

            // All the way round: the target circles the shoulder at the hand's distance (through the hand and the point
            // opposite it), pole out of the circle's plane. At most the opposite-aim crossing and the two bend
            // crossings may jump.
            let radius = a.distance(c);
            let circle: Vec<Vec3> = (0..=720)
                .map(|i| {
                    let angle = TAU * i as f32 / 720.0;
                    elbow_at(a + Vec3::new(angle.cos(), angle.sin(), 0.0) * radius, a + Vec3::Z, w)
                })
                .collect();
            let steps = jumps(&circle);
            eprintln!("weight {w}: largest jump round the circle {:.4} m", steps[0]);
            assert!(steps[3] < 0.06, "weight {w}: a fourth jump of {} m round the circle", steps[3]);
            assert!(steps[0] <= 2.0 * upper, "weight {w}: the crossing jump {} m is bounded by the limb", steps[0]);
        }
    }
}

#[test]
fn a_weight_fade_is_continuous_over_the_whole_range_for_every_fixed_target() {
    // The review's cases: the resting left arm (pointing along +X) with the target straight behind the shoulder (aim
    // exactly opposite the animated one) and the pole in front (bend side opposite too), reachable and far beyond reach
    // (a straight limb, whose bones would roll 180 degrees about the limb if the final rotations came from a shortest
    // arc); and the bent arm with the target on its own hand and the pole above the shoulder (bend side opposite). The
    // pose is a function of the weight and a fade must not jump anywhere in 0..=1, in joint positions AND in the
    // orientation of each bone, including the weights 0.707 and 1 that earlier blends broke at.
    for fx in both() {
        let rest = fx.ch.skeleton.rest_pose();
        let bones = LIMBS[0].1;
        let [a, b, c] = fx.joints(&rest, bones);
        assert!((b - a).dot(c - b) > 0.0, "the fixture arm is straight at rest");
        let bent = bent_left_arm(&fx);
        let [ba, _, bc] = fx.joints(&bent, bones);
        let reach = a.distance(b) + b.distance(c);
        let behind = a - (c - a).normalize();
        let cases = vec![
            (rest.clone(), a + (behind - a) * 0.364, a + Vec3::Z),
            (rest.clone(), a + (behind - a) * 3.0 * reach, a + Vec3::Y),
            (bent, bc, ba + Vec3::Y),
            (fx.starts().remove(2), a + (behind - a) * 0.364, a + Vec3::Z),
        ];
        let ids = fx.ids(bones);
        let at = |start: &Pose, target: Vec3, pole: Vec3, w: f32| {
            let mut pose = start.clone();
            TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
            let world = world(&fx.ch.skeleton, &pose);
            (fx.joints(&pose, bones), ids.map(|id| world.local[id.index()].rotation))
        };
        for (start, target, pole) in cases {
            let steps = 5000;
            let mut last = at(&start, target, pole, 0.0001);
            for i in 1..=steps {
                let w = 0.0001 + (1.0 - 0.0001) * i as f32 / steps as f32;
                let now = at(&start, target, pole, w);
                for (j, (p, q)) in now.0.iter().zip(&last.0).enumerate() {
                    assert!(p.distance(*q) < 0.01, "joint {j} jumped {} m at weight {w}", p.distance(*q));
                }
                for (j, (p, q)) in now.1.iter().zip(&last.1).enumerate() {
                    assert!(p.angle_between(*q) < 0.02, "bone {j} turned {} rad at weight {w}", p.angle_between(*q));
                }
                last = now;
            }
            let (low, high) = (at(&start, target, pole, 0.7070), at(&start, target, pole, 0.7072));
            assert!(low.0[2].distance(high.0[2]) < 1e-3, "no jump at 0.707");
            let (almost, full) = (at(&start, target, pole, 0.9999), at(&start, target, pole, 1.0));
            assert!(almost.0[2].distance(full.0[2]) < 2e-3, "the fade converges to the full solve");
            for (j, (p, q)) in almost.1.iter().zip(&full.1).enumerate() {
                assert!(
                    p.angle_between(*q) < 0.01,
                    "bone {j} flips {} rad at the end of the fade",
                    p.angle_between(*q)
                );
            }
            // And it starts from the animated pose: a hair of weight moves the tip a hair (the elbow of a straight limb
            // swings out by about sqrt(2·l·d) for a reach change d, so only the tip is bounded for every start).
            assert!(at(&start, target, pole, 0.0).0[2].distance(at(&start, target, pole, 1e-4).0[2]) < 1e-3);
        }
    }
}

/// World rotations of a limb's three bones.
fn rotations(fx: &Fixture, pose: &Pose, bones: [B; 3]) -> [Quat; 3] {
    let world = world(&fx.ch.skeleton, pose);
    fx.ids(bones).map(|id| world.local[id.index()].rotation)
}

/// How many consecutive samples have a bone turning by more than `limit` radians between them.
fn orientation_jumps(samples: &[[Quat; 3]], limit: f32) -> usize {
    samples.windows(2).filter(|w| w[0].iter().zip(&w[1]).any(|(p, q)| p.angle_between(*q) > limit)).count()
}

#[test]
fn a_straight_arm_keeps_its_bone_orientation_while_the_target_moves() {
    // The review's case: the resting (straight) left arm, full weight, pole along the arm, target about 90 degrees
    // away and moved a fraction of a millimetre each side of straight up. The wrist barely moves, and so must not any
    // bone's roll: a straight limb has no bend plane to take its roll from, so the roll is carried along the aim path.
    for fx in both() {
        let rest = fx.ch.skeleton.rest_pose();
        let bones = LIMBS[0].1;
        let [a, ..] = fx.joints(&rest, bones);
        let solved = |x: f32, pole: Vec3, w: f32| {
            let mut pose = rest.clone();
            let target = a + Vec3::new(x, 0.364, 0.0);
            TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
            (fx.joints(&pose, bones), rotations(&fx, &pose, bones))
        };
        for (pole, w) in [(a + Vec3::X, 1.0), (a + Vec3::X, 0.6), (a - Vec3::X, 1.0), (a + Vec3::Z, 1.0)] {
            let (low, high) = (solved(-0.0002, pole, w), solved(0.0002, pole, w));
            assert!(low.0[2].distance(high.0[2]) < 2e-3, "the wrist moves a hair");
            for (j, (p, q)) in low.1.iter().zip(&high.1).enumerate() {
                assert!(
                    p.angle_between(*q) < 0.01,
                    "bone {j} flips {} rad (pole {pole}, weight {w})",
                    p.angle_between(*q)
                );
            }
            let sweep: Vec<[Quat; 3]> = (0..=200).map(|i| solved(-0.01 + 0.0001 * i as f32, pole, w).1).collect();
            assert_eq!(orientation_jumps(&sweep, 0.01), 0, "pole {pole}, weight {w}");
        }
    }
}

#[test]
fn bone_orientation_is_smooth_round_a_target_circle_except_across_the_documented_opposite() {
    // Full and partial weight, straight and bent animated limbs, the target circling the shoulder at the hand's
    // distance: bone rotations change smoothly from sample to sample (< 0.05 rad per 0.5 degrees) except at the few
    // crossings: the target passing the exact opposite of the animated aim or bend, and an in-plane pole lying on the
    // line to the target (twice a circle), where the side has to be chosen afresh (at most four in all, as measured).
    for fx in both() {
        let bones = LIMBS[0].1;
        for bend in [false, true] {
            let start = if bend { bent_left_arm(&fx) } else { fx.ch.skeleton.rest_pose() };
            let [a, _, c] = fx.joints(&start, bones);
            let radius = a.distance(c).max(0.3);
            for w in [0.25, 0.5, 0.9, 1.0] {
                for pole in [a + Vec3::Z, a + Vec3::Y, a + Vec3::X] {
                    let samples: Vec<[Quat; 3]> = (0..=720)
                        .map(|i| {
                            let angle = TAU * i as f32 / 720.0;
                            let mut pose = start.clone();
                            let target = a + Vec3::new(angle.cos(), angle.sin(), 0.0) * radius;
                            TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
                            rotations(&fx, &pose, bones)
                        })
                        .collect();
                    let jumps = orientation_jumps(&samples, 0.05);
                    assert!(jumps <= 4, "{jumps} bone-orientation jumps (weight {w}, pole {pole})");
                }
            }
        }
    }
}

#[test]
fn bone_orientation_follows_the_target_over_a_sphere_of_directions_at_full_weight() {
    // Full weight, the target on a sphere around the shoulder (every 10 degrees of polar angle, swept in azimuth every
    // degree), straight and bent animated arms, a pole along the arm and two across it. Between neighbouring targets
    // a bone's orientation may change by about the target's own step; near the line of the pole the bend side
    // legitimately turns faster, by the inverse sine of the angle to that line, and so does a roll transported to near the
    // exact opposite of the animated aim (the same singularity, fading with distance). Excluded: within a degree of the
    // exact opposite of the animated aim (the documented exception) and of the pole line itself (no side to take).
    for fx in both() {
        let bones = LIMBS[0].1;
        for bend in [false, true] {
            let start = if bend { bent_left_arm(&fx) } else { fx.ch.skeleton.rest_pose() };
            let [a, _, c] = fx.joints(&start, bones);
            let animated = (c - a).normalize();
            for pole_dir in [Vec3::X, Vec3::Z, Vec3::Y] {
                let sample = |polar: f32, azimuth: f32| {
                    let dir = Vec3::new(polar.sin() * azimuth.cos(), polar.cos(), polar.sin() * azimuth.sin());
                    let mut pose = start.clone();
                    TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(a + dir * 0.364, a + pole_dir, 1.0)), &mut pose);
                    (dir, rotations(&fx, &pose, bones))
                };
                let (mut worst, mut checked) = (0.0_f32, 0);
                for ring in 1..18 {
                    let polar = ring as f32 * 10.0_f32.to_radians();
                    let mut last = sample(polar, 0.0);
                    for step in 1..=360 {
                        let now = sample(polar, (step as f32).to_radians());
                        let target_step = last.0.angle_between(now.0);
                        let to_pole = now.0.angle_between(pole_dir).min(now.0.angle_between(-pole_dir));
                        let near_exception =
                            now.0.angle_between(-animated) < 1.0_f32.to_radians() || to_pole < 1.0_f32.to_radians();
                        let near_last = last.0.angle_between(-animated) < 1.0_f32.to_radians()
                            || last.0.angle_between(pole_dir).min(last.0.angle_between(-pole_dir))
                                < 1.0_f32.to_radians();
                        if !(near_exception || near_last) {
                            let to_opposite = now.0.angle_between(-animated);
                            let allowed = (3.0 + 1.0 / to_pole.sin() + 3.0 / to_opposite.min(FRAC_PI_2).sin())
                                * target_step
                                + 1e-3;
                            for (j, (p, q)) in now.1.iter().zip(&last.1).enumerate() {
                                let turned = p.angle_between(*q);
                                worst = worst.max(turned / allowed);
                                assert!(
                                    turned <= allowed,
                                    "bone {j} turned {turned} rad for a {target_step} rad target step at polar {polar}, azimuth {step} (pole {pole_dir}, bent {bend})"
                                );
                            }
                            checked += 1;
                        }
                        last = now;
                    }
                }
                assert!(checked > 5000, "the sweep covered the sphere ({checked} steps)");
                eprintln!(
                    "sphere sweep: pole {pole_dir}, bent {bend}: worst {worst:.3} of the allowed turn over {checked} steps"
                );
            }
        }
    }
}

/// The fixture with the given upper arm and forearm lengths on both sides (their directions kept).
fn with_arm_lengths(base: &Fixture, upper: f32, lower: f32) -> Fixture {
    let mut bones: Vec<BoneDef> = base.ch.skeleton.bones().to_vec();
    for def in &mut bones {
        let length = match def.humanoid {
            Some(B::LeftLowerArm | B::RightLowerArm) => upper,
            Some(B::LeftHand | B::RightHand) => lower,
            _ => continue,
        };
        def.rest.translation = def.rest.translation.normalize() * length;
    }
    let skeleton = Skeleton::new(bones).unwrap();
    let ch = common::Character {
        skeleton: skeleton.clone(),
        rest_pos: base.ch.rest_pos.clone(),
        rest_rot: base.ch.rest_rot.clone(),
    };
    Fixture { ch, rig: HumanoidRig::new(RigSkeleton::new(skeleton)) }
}

#[test]
fn a_weight_fade_keeps_bone_orientation_for_unequal_segments_and_a_deeply_bent_elbow() {
    // The review's case: a 0.30 m upper arm, a 0.20 m forearm, the elbow bent 140 degrees. The forearm then leans past
    // 90 degrees from the aim, and a flipped bend side puts its old and new directions nearly opposite, where a
    // shortest-arc rotation has no answer. Fixed target and pole, weight swept over 0..=1: nothing may flip.
    let mut rng = Rng::new(77);
    for base in both() {
        let fx = with_arm_lengths(&base, 0.30, 0.20);
        let start = bent_left_arm(&fx);
        let bones = LIMBS[0].1;
        let [a, b, c] = fx.joints(&start, bones);
        assert!((a.distance(b) - 0.30).abs() < 1e-4 && (b.distance(c) - 0.20).abs() < 1e-4, "segment lengths");
        assert!((flexion([a, b, c]).to_degrees() - 140.0).abs() < 0.5, "the elbow starts bent 140 degrees");
        for _ in 0..120 {
            let target = a + random_dir(&mut rng) * rng.range_f32(0.12, 0.5);
            let pole = a + random_dir(&mut rng);
            let at = |w: f32| {
                let mut pose = start.clone();
                TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
                (fx.joints(&pose, bones), rotations(&fx, &pose, bones))
            };
            let mut last = at(0.001);
            for i in 1..=500 {
                let now = at(0.001 + 0.999 * i as f32 / 500.0);
                for (j, (p, q)) in now.1.iter().zip(&last.1).enumerate() {
                    assert!(
                        p.angle_between(*q) < 0.08,
                        "bone {j} turned {} rad (target {target}, pole {pole})",
                        p.angle_between(*q)
                    );
                }
                last = now;
            }
            let (almost, full) = (at(0.999), at(1.0));
            assert!(almost.0[2].distance(full.0[2]) < 2e-3, "the wrist moves a hair");
            for (j, (p, q)) in almost.1.iter().zip(&full.1).enumerate() {
                assert!(
                    p.angle_between(*q) < 0.02,
                    "bone {j} flips {} rad at the end of the fade",
                    p.angle_between(*q)
                );
            }
        }
    }
}

/// The resting pose with the left elbow turned by `elbow` (normalised space), through the humanoid stage.
fn elbow_pose(fx: &Fixture, elbow: Quat) -> Pose {
    let mut humanoid = HumanoidPose::REST;
    humanoid.set(B::LeftLowerArm, elbow);
    let mut pose = Pose::IDENTITY;
    fx.rig.apply_humanoid(&humanoid, &mut pose);
    pose
}

#[test]
fn bone_roll_is_continuous_as_the_animated_elbow_straightens_and_its_bend_turns() {
    // The review's case: weight 1, target along the arm, pole across it, the animated elbow turned 0.0007 then 0.0009
    // rad. The solved wrist barely moves, so no bone may roll: the direction of a bend that shrinks to nothing has no
    // limit, so the animated plane may not be the roll reference once the bend is small.
    for fx in both() {
        let bones = LIMBS[0].1;
        let rest = fx.ch.skeleton.rest_pose();
        let [a, ..] = fx.joints(&rest, bones);
        let solved = |pose: &Pose, w: f32| {
            let mut out = pose.clone();
            let goal = limb(a + Vec3::X * 0.364, a + Vec3::Z, w);
            TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, goal), &mut out);
            (fx.joints(&out, bones), rotations(&fx, &out, bones))
        };
        for w in [1.0, 0.3] {
            let (low, high) = (
                solved(&elbow_pose(&fx, Quat::from_rotation_z(0.0007)), w),
                solved(&elbow_pose(&fx, Quat::from_rotation_z(0.0009)), w),
            );
            assert!(low.0[2].distance(high.0[2]) < 1e-3, "the wrist moves a hair");
            for (j, (p, q)) in low.1.iter().zip(&high.1).enumerate() {
                assert!(p.angle_between(*q) < 0.01, "bone {j} rolls {} rad at weight {w}", p.angle_between(*q));
            }

            // The elbow swept through straight and out to a clear bend on either side: bones turn smoothly with it.
            let hinge: Vec<[Quat; 3]> = (0..=600)
                .map(|i| solved(&elbow_pose(&fx, Quat::from_rotation_z(-0.3 + 0.001 * i as f32)), w).1)
                .collect();
            assert_eq!(orientation_jumps(&hinge, 0.05), 0, "elbow sweep, weight {w}");

            // A small fixed bend whose direction turns all the way round the limb.
            let round: Vec<[Quat; 3]> = (0..=360)
                .map(|i| {
                    let heading = (i as f32).to_radians();
                    let axis = Vec3::new(0.0, heading.cos(), heading.sin());
                    solved(&elbow_pose(&fx, Quat::from_axis_angle(axis, 0.04)), w).1
                })
                .collect();
            // At full weight the roll follows the heading smoothly. At a partial weight the bend heading passes once
            // through the exact opposite of the solved plane (singular set 1, documented in the module docs), where the
            // elbow goes the other way round: at most two samples may jump there. Elsewhere the roll reference blends
            // the animated plane with the solved one, so it turns about 1 / |1 - 2·τ| times faster than the heading
            // (τ about 0.36 here): 0.1 rad per degree.
            let allowed = 2;
            assert!(orientation_jumps(&round, 0.1) <= allowed, "bend direction sweep, weight {w}");
        }
    }
}

#[test]
fn the_faded_in_animated_bend_has_one_bounded_singular_set() {
    // The second allowed singular set: the animated bend is partly faded in (below `NEAR_STRAIGHT`, the sine of the elbow's flexion, its plane is swept
    // toward the solved one) and its side is exactly opposite the solved plane. As the bend direction turns once round
    // the limb the roll reference crosses that opposite once, and jumps by `2π · min(τ, 1 - τ)` where `τ` is how far it
    // is swept (here `τ = 1 - share` at weight 1). Everywhere else it is continuous, with a gain of at most about one.
    let fx = Fixture::new(false);
    let bones = LIMBS[0].1;
    let rest = fx.ch.skeleton.rest_pose();
    let [a, ..] = fx.joints(&rest, bones);
    // The animated elbow flexion whose bend fades in by `curvature` (0..1): found from the pose itself.
    let flexion_for = |curvature: f32| {
        let measured = |f: f32| {
            let pose = elbow_pose(&fx, Quat::from_rotation_z(f));
            let [shoulder, elbow, wrist] = fx.joints(&pose, bones);
            let aim = (wrist - shoulder).normalize();
            let offset = (elbow - shoulder) - aim * (elbow - shoulder).dot(aim);
            offset.length() / shoulder.distance(elbow).min(elbow.distance(wrist)) / NEAR_STRAIGHT
        };
        let (mut low, mut high) = (0.0_f32, 0.6_f32);
        for _ in 0..40 {
            let mid = f32::midpoint(low, high);
            if measured(mid) < curvature { low = mid } else { high = mid }
        }
        f32::midpoint(low, high)
    };
    let solved = |flexion: f32, heading: f32, pole: Vec3| {
        let axis = Vec3::new(0.0, heading.cos(), heading.sin());
        let pose = elbow_pose(&fx, Quat::from_axis_angle(axis, flexion));
        let mut out = pose;
        TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(a + Vec3::X * 0.364, pole, 1.0)), &mut out);
        rotations(&fx, &out, bones)
    };
    // The solved plane for this target and pole is the one the pole names: a pole along +Z gives a bend toward +Z.
    let pole = a + Vec3::Z;
    for (curvature, label) in [(0.5, "share one half"), (0.4, "share 0.35"), (0.2, "share 0.10")] {
        let flexion = flexion_for(curvature);
        let step = 0.01_f32.to_radians();
        let samples: Vec<[Quat; 3]> = (0..=36_000).map(|i| solved(flexion, step * i as f32, pole)).collect();
        let mut steps: Vec<f32> = samples
            .windows(2)
            .map(|w| w[0].iter().zip(&w[1]).map(|(p, q)| p.angle_between(*q)).fold(0.0, f32::max))
            .collect();
        steps.sort_by(|x, y| y.total_cmp(x));
        let share = curvature * curvature * (3.0 - 2.0 * curvature);
        let tau = 1.0 - share;
        let expected = TAU * tau.min(1.0 - tau);
        eprintln!(
            "{label}: the crossing turns {:.3} rad (expected {expected:.3}); the next largest step is {:.4} rad (heading step {step:.5} rad)",
            steps[0], steps[1]
        );
        assert!((steps[0] - expected).abs() < 0.1, "{label}: the crossing turns {} rad, expected {expected}", steps[0]);
        assert!(steps[0] <= PI + 1e-3, "and never more than half a turn");
        assert!(steps[1] < 3.0 * step + 0.002, "{label}: everywhere else continuous, gain {}", steps[1] / step);
    }
}

#[test]
fn a_weight_fade_from_a_nearly_straight_animated_elbow_starts_at_the_animated_pose() {
    // The review's case: the animated elbow turned 0.05 rad (a bend that is partly faded in), target along the arm, pole
    // across it. A hair of weight must leave the elbow and every bone where the animation put them, and the fade must
    // then move smoothly to the full solve, for every size of bend from straight to clearly bent.
    for fx in both() {
        let bones = LIMBS[0].1;
        let rest = fx.ch.skeleton.rest_pose();
        let [a, ..] = fx.joints(&rest, bones);
        for flexion in [0.0007, 0.01, 0.05, 0.2, 0.6] {
            let start = elbow_pose(&fx, Quat::from_rotation_z(flexion));
            let at = |w: f32| {
                let mut pose = start.clone();
                TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(a + Vec3::X * 0.364, a + Vec3::Z, w)), &mut pose);
                (fx.joints(&pose, bones), rotations(&fx, &pose, bones))
            };
            let (animated, hair) = (at(0.0), at(1e-8));
            for (j, (p, q)) in animated.0.iter().zip(&hair.0).enumerate() {
                assert!(
                    p.distance(*q) < 1e-4,
                    "joint {j} moves {} m at weight 1e-8 (flexion {flexion})",
                    p.distance(*q)
                );
            }
            for (j, (p, q)) in animated.1.iter().zip(&hair.1).enumerate() {
                assert!(
                    p.angle_between(*q) < 1e-3,
                    "bone {j} turns {} rad at weight 1e-8 (flexion {flexion})",
                    p.angle_between(*q)
                );
            }
            let mut last = at(1e-8);
            for i in 1..=500 {
                let now = at(i as f32 / 500.0);
                for (j, (p, q)) in now.0.iter().zip(&last.0).enumerate() {
                    assert!(p.distance(*q) < 0.02, "joint {j} jumps {} m (flexion {flexion})", p.distance(*q));
                }
                for (j, (p, q)) in now.1.iter().zip(&last.1).enumerate() {
                    assert!(
                        p.angle_between(*q) < 0.1,
                        "bone {j} turns {} rad (flexion {flexion})",
                        p.angle_between(*q)
                    );
                }
                last = now;
            }
        }
    }
}

#[test]
fn a_weight_fade_never_pops_with_the_animated_bend_opposite_the_solved_plane() {
    // The review's case: the animated elbow turned 0.03 rad about Z, the target along the animated shoulder-wrist
    // direction, the pole on the side opposite the animated bend. For a fixed target and pole the pose is a function of
    // the weight and must not jump anywhere in 0..=1, including where the roll reference would be half way between
    // the two opposite planes (`(1 - fade) · sweep = 1/2`, near weight 0.25 here).
    for fx in both() {
        let bones = LIMBS[0].1;
        let rest = fx.ch.skeleton.rest_pose();
        let [a, ..] = fx.joints(&rest, bones);
        for flexion in [0.03, 0.01, 0.1] {
            let start = elbow_pose(&fx, Quat::from_rotation_z(flexion));
            let [shoulder, _, wrist] = fx.joints(&start, bones);
            let target = shoulder + (wrist - shoulder).normalize() * 0.364;
            for pole in [a + Vec3::Y, a - Vec3::Y, a + Vec3::Z, a - Vec3::Z] {
                let at = |w: f32| {
                    let mut pose = start.clone();
                    TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
                    (fx.joints(&pose, bones), rotations(&fx, &pose, bones))
                };
                let mut last = at(1e-6);
                for i in 1..=4000 {
                    let w = 1e-6 + (1.0 - 1e-6) * i as f32 / 4000.0;
                    let now = at(w);
                    for (j, (p, q)) in now.0.iter().zip(&last.0).enumerate() {
                        assert!(
                            p.distance(*q) < 0.01,
                            "joint {j} jumps {} m at weight {w} (flexion {flexion}, pole {pole})",
                            p.distance(*q)
                        );
                    }
                    for (j, (p, q)) in now.1.iter().zip(&last.1).enumerate() {
                        assert!(
                            p.angle_between(*q) < 0.02,
                            "bone {j} turns {} rad at weight {w} (flexion {flexion}, pole {pole})",
                            p.angle_between(*q)
                        );
                    }
                    last = now;
                }
            }
        }
    }
}

#[test]
fn a_weight_fade_never_pops_for_a_very_short_forearm() {
    // The review's case: a 0.30 m upper arm and a 5 mm forearm, the animated elbow bent about 91 degrees (so the forearm
    // sticks out perpendicular to the aim), the target along the animated aim, the pole turned 150 degrees from the
    // animated bend. The bend must be judged by the elbow's flexion, not by the upper bone's lean from the aim, which
    // stays small here and would fade the roll reference while the forearm lies across it. Swept over forearm lengths,
    // elbow angles and pole angles, with a weight fade of 1500 steps each.
    for base in both() {
        for forearm in [0.005, 0.02, 0.1] {
            let fx = with_arm_lengths(&base, 0.30, forearm);
            let bones = LIMBS[0].1;
            for degrees in [60.0_f32, 90.955, 120.0] {
                let start = elbow_pose(&fx, Quat::from_rotation_z(degrees.to_radians()));
                let [shoulder, elbow, wrist] = fx.joints(&start, bones);
                let aim = (wrist - shoulder).normalize();
                let bend = ((elbow - shoulder) - aim * (elbow - shoulder).dot(aim)).normalize();
                let target = shoulder + aim * 0.298 * ((wrist - shoulder).length() / 0.305).min(1.0);
                for turn in [0.0_f32, 60.0, 150.0, 210.0, 300.0] {
                    let pole = shoulder + Quat::from_axis_angle(aim, turn.to_radians()) * bend;
                    let at = |w: f32| {
                        let mut pose = start.clone();
                        TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
                        (fx.joints(&pose, bones), rotations(&fx, &pose, bones))
                    };
                    let mut last = at(1e-6);
                    for i in 1..=1500 {
                        let w = 1e-6 + (1.0 - 1e-6) * i as f32 / 1500.0;
                        let now = at(w);
                        let context = format!("weight {w}, forearm {forearm}, elbow {degrees}, pole turn {turn}");
                        for (j, (p, q)) in now.0.iter().zip(&last.0).enumerate() {
                            assert!(p.distance(*q) < 0.01, "joint {j} jumps {} m ({context})", p.distance(*q));
                        }
                        for (j, (p, q)) in now.1.iter().zip(&last.1).enumerate() {
                            assert!(
                                p.angle_between(*q) < 0.02,
                                "bone {j} turns {} rad ({context})",
                                p.angle_between(*q)
                            );
                        }
                        last = now;
                    }
                }
            }
        }
    }
}

/// Joint positions and bone orientations of a limb, in one forward-kinematics pass.
fn limb_state(fx: &Fixture, pose: &Pose, bones: [B; 3]) -> ([Vec3; 3], [Quat; 3]) {
    let world = world(&fx.ch.skeleton, pose);
    let ids = fx.ids(bones);
    (ids.map(|id| world.local[id.index()].translation), ids.map(|id| world.local[id.index()].rotation))
}

/// How far apart two limb states are: the largest bone turn (rad) and the largest joint move (m).
fn state_gap(a: &([Vec3; 3], [Quat; 3]), b: &([Vec3; 3], [Quat; 3])) -> (f32, f32) {
    let turn = a.1.iter().zip(&b.1).map(|(p, q)| p.angle_between(*q)).fold(0.0, f32::max);
    let moved = a.0.iter().zip(&b.0).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max);
    (turn, moved)
}

type LimbState = ([Vec3; 3], [Quat; 3]);

/// The size of a gap between two limb states in units of the thresholds that make a step worth bisecting (0.05 rad of
/// bone turn, 2 cm of joint move).
fn gap_size(a: &LimbState, b: &LimbState) -> f32 {
    let (turn, moved) = state_gap(a, b);
    (turn / 0.05).max(moved / 0.02)
}

/// Bisects the weight interval `lo..hi` towards the larger half of the gap until the interval cannot be split or is
/// narrower than 1e-12, and returns the final interval's width and its bone turn and joint move. A jump keeps its size
/// however narrow the interval; a steep slope, even a square-root one, shrinks.
fn settle(at: &impl Fn(f32) -> LimbState, mut lo: (f32, LimbState), mut hi: (f32, LimbState)) -> (f32, f32, f32) {
    while hi.0 - lo.0 > 1e-12 {
        let mid_w = f32::midpoint(lo.0, hi.0);
        if mid_w <= lo.0 || mid_w >= hi.0 {
            break;
        }
        let mid = (mid_w, at(mid_w));
        if gap_size(&lo.1, &mid.1) >= gap_size(&mid.1, &hi.1) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let (turn, moved) = state_gap(&lo.1, &hi.1);
    ((hi.0 - lo.0).max(1e-12), turn, moved)
}

/// One randomised arm, animated pose, target and pole for the weight-fade property test.
struct FadeCase {
    fx: Fixture,
    slot: usize,
    bones: [B; 3],
    start: Pose,
    target: Vec3,
    pole: Vec3,
    description: String,
}

/// Case number `case` of seed `seed`: reproducible from those two numbers alone.
fn fade_case(bases: &[Fixture; 2], seed: u64, case: u64) -> FadeCase {
    let mut rng = Rng::new(seed).fork(case);
    let base = &bases[(case % 2) as usize];
    let limb_index = ((case / 2) % 2) as usize;
    let (slot, bones, _) = LIMBS[limb_index];
    // Segment lengths: the longer in 0.15..0.45 m, the shorter up to 60 times smaller.
    let longer = rng.range_f32(0.15, 0.45);
    // One case in six has nearly equal segments, 1e-7..1e-3 m apart, where the triangle is badly conditioned near a fold.
    let shorter = if rng.next_f32() < 1.0 / 6.0 {
        longer - 10.0_f32.powf(rng.range_f32(-7.0, -3.0))
    } else {
        longer / rng.range_f32(0.0, 60.0_f32.ln()).exp()
    };
    let (upper, lower) = if rng.next_f32() < 0.5 { (longer, shorter) } else { (shorter, longer) };
    let fx = with_arm_lengths(base, upper, lower);
    // The animated pose: the whole arm turned at random, the elbow bent straight, a hair, or up to folded flat.
    let mut humanoid = HumanoidPose::REST;
    let sides = if limb_index == 0 { (B::LeftUpperArm, B::LeftLowerArm) } else { (B::RightUpperArm, B::RightLowerArm) };
    humanoid.set(sides.0, Quat::from_axis_angle(random_dir(&mut rng), rng.range_f32(0.0, 1.2)));
    let flexion = match (rng.next_f32() * 8.0) as u32 {
        0 => 0.0,
        1 => rng.range_f32(1e-4, 1e-2),
        2 => rng.range_f32(1e-2, 0.2),
        3 => PI,
        4 => PI - rng.range_f32(0.0, 0.05),
        _ => rng.range_f32(0.0, PI),
    };
    let heading = rng.range_f32(0.0, TAU);
    humanoid.set(sides.1, Quat::from_axis_angle(Vec3::new(0.0, heading.cos(), heading.sin()), flexion));
    let mut start = Pose::IDENTITY;
    fx.rig.apply_humanoid(&humanoid, &mut start);
    let (joints, _) = limb_state(&fx, &start, bones);
    let reach_share = match (rng.next_f32() * 3.0) as u32 {
        0 => rng.range_f32(0.05, 1.0),
        1 => 1.0,
        _ => rng.range_f32(1.0, 1.5),
    };
    let target = joints[0] + random_dir(&mut rng) * (upper + lower) * reach_share;
    let pole = joints[0] + random_dir(&mut rng);
    let description = format!(
        "seed {seed:#x} case {case}: arm {limb_index}, segments {upper:.4}/{lower:.4}, elbow {:.2} deg, \
         target {reach_share:.3} of reach, target {target}, pole {pole}",
        flexion.to_degrees()
    );
    FadeCase { fx, slot, bones, start, target, pole, description }
}

impl FadeCase {
    /// The limb after solving at `weight`.
    fn at(&self, weight: f32) -> ([Vec3; 3], [Quat; 3]) {
        let mut pose = self.start.clone();
        let goal = limb(self.target, self.pole, weight);
        TwoBoneIk.solve(&self.fx.ch.skeleton, &goals(self.slot, goal), &mut pose);
        limb_state(&self.fx, &pose, self.bones)
    }
}

#[test]
fn a_weight_fade_never_jumps_over_randomised_arms_bends_targets_and_poles() {
    // The property behind every weight-fade review round: for any arm (segment ratios from 1/60 to 60), any animated
    // elbow (straight to folded), any target (inside, at and beyond reach), any pole, on either arm of either fixture,
    // the pose is a continuous function of the weight. Each case sweeps the weight 0..1 in 500 steps; a step that turns a
    // bone by more than 0.05 rad (or moves a joint more than 2 cm) is bisected until the interval cannot be split: a jump
    // keeps its size, a steep slope (even a square-root one) shrinks with the width and is only recorded. Only jumps fail. Reproduce a failure with the
    // seed and case number printed in the message.
    const SEED: u64 = 0x0921_1489;
    const CASES: u64 = 2_000;
    let bases = both();
    let (mut worst_gain, mut worst_case, mut bisected) = (0.0_f32, 0_u64, 0_u32);
    for case in 0..CASES {
        let c = fade_case(&bases, SEED, case);
        // A hair of weight leaves the wrist where the animation put it. (The elbow may swing: with bones that nearly
        // match, folded flat, it moves as the square root of the tip's move, a steep slope that `settle` tells from a jump.)
        let (animated, hair) = (c.at(0.0), c.at(1e-9));
        let wrist_moved = animated.0[2].distance(hair.0[2]);
        // 5 micrometres: world positions are known to about 2e-6 m, and a limb folded flat has its wrist re-placed
        // along a substituted aim within that distance of where it was.
        assert!(wrist_moved < 5e-6, "weight 1e-9 moves the wrist {wrist_moved} m ({})", c.description);
        let steps = 500;
        let mut last = (0.0_f32, animated);
        for i in 1..=steps {
            let w = i as f32 / steps as f32;
            let now = (w, c.at(w));
            let (turn, moved) = state_gap(&last.1, &now.1);
            if turn > 0.05 || moved > 0.02 {
                bisected += 1;
                let first = gap_size(&last.1, &now.1);
                let (width, final_turn, final_moved) = settle(&|x| c.at(x), last, now);
                let remaining = (final_turn / 0.05).max(final_moved / 0.02);
                assert!(
                    remaining < 0.1 * first || (final_turn < 0.02 && final_moved < 0.004),
                    "a weight jump of {final_turn} rad / {final_moved} m survives bisection to width {width} at weight {} ({})",
                    last.0,
                    c.description
                );
                let gain = final_turn.max(final_moved / 0.4) / width;
                if gain > worst_gain {
                    (worst_gain, worst_case) = (gain, case);
                }
            }
            last = now;
        }
    }
    eprintln!(
        "{CASES} randomised weight fades (seed {SEED:#x}): {bisected} steep steps bisected, largest gain {worst_gain:.1} per unit weight ({})",
        fade_case(&bases, SEED, worst_case).description
    );
}

#[test]
fn a_weight_fade_never_pops_with_the_elbow_folded_nearly_flat() {
    // The review's case: shoulder at the origin, elbow 0.25 m out and 1/256 m up, wrist 0.25 m back and 1/256 m up
    // (179.1 degrees of flexion), target 0.298 m along the shoulder-wrist direction, the pole 150 degrees round from the
    // elbow. With long bones the limb is folded tight but its plane is perfectly defined (the elbow stands 0.25 m off
    // the line), so it must not fade its roll reference as a straight limb does. Swept over folds from 170 to exactly 180
    // degrees and several poles, with a weight fade of 4000 steps each.
    for base in both() {
        let fx = with_arm_lengths(&base, 0.25, 0.25);
        let bones = LIMBS[0].1;
        for degrees in [170.0_f32, 179.105, 179.9, 180.0] {
            let start = elbow_pose(&fx, Quat::from_rotation_z(degrees.to_radians()));
            let [shoulder, elbow, wrist] = fx.joints(&start, bones);
            // Folded exactly flat the wrist is on the shoulder and the shoulder-wrist vector is rounding noise, so the
            // aim (and with it the pole's reference) is taken perpendicular to the bones.
            let aim = if shoulder.distance(wrist) > 1e-3 {
                (wrist - shoulder).normalize()
            } else {
                let upper = (elbow - shoulder).normalize();
                (Vec3::Y - upper * Vec3::Y.dot(upper)).normalize()
            };
            let bend = ((elbow - shoulder) - aim * (elbow - shoulder).dot(aim)).normalize();
            let target = shoulder + aim * 0.298;
            for turn in [0.0_f32, 60.0, 150.0, 210.0, 300.0] {
                let pole = shoulder + Quat::from_axis_angle(aim, turn.to_radians()) * bend;
                let at = |w: f32| {
                    let mut pose = start.clone();
                    TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
                    limb_state(&fx, &pose, bones)
                };
                let mut last = at(1e-6);
                for i in 1..=4000 {
                    let w = 1e-6 + (1.0 - 1e-6) * i as f32 / 4000.0;
                    let now = at(w);
                    let (turn_rad, moved) = state_gap(&last, &now);
                    assert!(
                        moved < 0.01,
                        "joint jumps {moved} m at weight {w} (fold {degrees}, pole turn {turn}): {:?} -> {:?}",
                        last.0,
                        now.0
                    );
                    assert!(
                        turn_rad < 0.02,
                        "bone turns {turn_rad} rad at weight {w} (fold {degrees}, pole turn {turn})"
                    );
                    last = now;
                }
            }
        }
    }
}

#[test]
fn a_wrist_on_the_shoulder_with_the_target_along_the_elbow_solves_deterministically() {
    // The review's geometry: equal bones folded flat (the wrist on the shoulder), the target straight along the upper
    // bone. Projecting it off the bone leaves only rounding noise, which must not become the aim. Weight-zero
    // continuity is not promised here (singular set 3: the wrist moves to `w * T` and the elbow has to be on the circle
    // perpendicular to `T`, about 90 degrees from where it was), so this checks what is promised: the solve does not
    // depend on the noise (the elbow at weight 0.5 agrees to 1 mm when the pose is perturbed by an ulp), it reaches
    // the target at weight 1, and nothing is NaN. Swept over 72 upper-arm directions.
    for base in both() {
        let fx = with_arm_lengths(&base, 0.25, 0.25);
        let bones = LIMBS[0].1;
        for step in 0..72 {
            let degrees = step as f32 * 5.0;
            let state = |nudge: Quat| {
                let mut humanoid = HumanoidPose::REST;
                humanoid.set(
                    B::LeftUpperArm,
                    Quat::from_rotation_z(degrees.to_radians()) * Quat::from_rotation_x(0.3 * degrees.to_radians()),
                );
                humanoid.set(B::LeftLowerArm, nudge * Quat::from_rotation_z(180.0_f32.to_radians()));
                let mut start = Pose::IDENTITY;
                fx.rig.apply_humanoid(&humanoid, &mut start);
                start
            };
            let start = state(Quat::IDENTITY);
            let [shoulder, elbow, _] = fx.joints(&start, bones);
            let upper = (elbow - shoulder).normalize();
            let (target, pole) = (shoulder + upper * 0.298, shoulder + upper.any_orthonormal_vector());
            let solved = |nudge: Quat, w: f32| {
                let mut pose = state(nudge);
                TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
                fx.joints(&pose, bones)
            };
            let reference = solved(Quat::IDENTITY, 0.5);
            for nudge in [
                Quat::from_rotation_x(1.2e-7),
                Quat::from_rotation_y(-1.2e-7),
                Quat::from_rotation_z(1.2e-7),
                Quat::from_euler(omuzaki_core::glam::EulerRot::XYZ, 1.2e-7, 1.2e-7, -1.2e-7),
            ] {
                for w in [0.5_f32, 1.0] {
                    let joints = solved(nudge, w);
                    assert!(joints.iter().all(|j| j.is_finite()), "NaN at weight {w}, upper arm {degrees}");
                    if w == 1.0 {
                        assert!(joints[2].distance(target) < 1e-3, "weight 1 misses the target (upper arm {degrees})");
                    } else {
                        let moved = joints[1].distance(reference[1]);
                        assert!(moved < 1e-3, "the elbow moves {moved} m under an ulp (upper arm {degrees})");
                    }
                }
            }
        }
    }
}

#[test]
fn a_weight_fade_starts_at_the_animated_pose_for_nearly_equal_folded_bones() {
    // The review's case: a 0.25 m upper arm and a 0.24999 m forearm folded to 180 degrees, so the wrist is 1e-5 m from
    // the shoulder, along the bones. That shoulder-wrist vector is a real direction, and the triangle (nearly equal
    // sides, a tiny base) is a square-root singularity: the elbow's height is the square root of `l1² - along²`, so
    // moving the tip a nanometre swings the elbow by millimetres. That is a steep slope, not a jump: the wrist is
    // continuous from the first hair of weight, and the elbow's gap shrinks as the weight interval narrows. Swept over
    // how nearly equal the bones are, either one longer, and how far folded.
    for base in both() {
        for (upper, forearm) in [
            (0.25, 0.24999),
            (0.24999, 0.25),
            (0.25, 0.249_999),
            (0.25, 0.249_999_9),
            (0.25, 0.249),
            (0.25, 0.25),
            // F / U = 1.0001, 0.9999 and 0.999
            (0.25, 0.250_025),
            (0.25, 0.249_975),
            (0.25, 0.249_75),
        ] {
            let fx = with_arm_lengths(&base, upper, forearm);
            let bones = LIMBS[0].1;
            for degrees in [180.0_f32, 179.999, 179.9, 179.0] {
                let start = elbow_pose(&fx, Quat::from_rotation_z(degrees.to_radians()));
                let [shoulder, ..] = fx.joints(&start, bones);
                let (target, pole) = (shoulder + Vec3::Y * 0.298, shoulder + Vec3::Z);
                let at = |w: f32| {
                    let mut pose = start.clone();
                    TwoBoneIk.solve(&fx.ch.skeleton, &goals(0, limb(target, pole, w)), &mut pose);
                    limb_state(&fx, &pose, bones)
                };
                let context = format!("segments {upper}/{forearm}, fold {degrees}");
                let animated = at(0.0);
                for w in [1e-10_f32, 1e-8, 1e-6, 1e-4] {
                    let wrist = animated.0[2].distance(at(w).0[2]);
                    assert!(wrist < 1e-5 + 2.0 * w, "weight {w} moves the wrist {wrist} m ({context})");
                }
                // Weights from a hair up to one: geometric steps to 1e-3, then linear. Any step that is a big gap is
                // bisected, and its gap must shrink: only a gap that does not shrink is a jump.
                let mut weights: Vec<f32> = (0..=7).map(|e| 1e-10 * 10.0_f32.powi(e)).collect();
                weights.extend((1..=1000).map(|i| 1e-3 + (1.0 - 1e-3) * i as f32 / 1000.0));
                let mut last = (0.0_f32, animated);
                for w in weights {
                    let now = (w, at(w));
                    let first = gap_size(&last.1, &now.1);
                    if first > 1.0 {
                        let (width, turn, moved) = settle(&at, last, now);
                        let remaining = (turn / 0.05).max(moved / 0.02);
                        assert!(
                            remaining < 0.1 * first || (turn < 0.02 && moved < 0.004),
                            "a jump of {turn} rad / {moved} m survives bisection to width {width} at weight {} ({context})",
                            last.0
                        );
                    }
                    last = now;
                }
            }
        }
    }
}
