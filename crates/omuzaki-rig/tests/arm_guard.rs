//! The arm guard (#2353): forearms and hands driven into the chest, a thigh and the face end the frame outside the
//! body; an arm that is clear is untouched; a push lets go smoothly; the body is measured from a skinned mesh; and a
//! frame allocates nothing and costs microseconds.
//!
//! The body is a plain adult in metres (T-pose, arms along X). Its "mesh" is points on the surfaces of capsules of
//! known radii, each carried by one bone, so the fit has an exact answer.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::float_cmp, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use omuzaki_anim_api::{BoneId, HumanoidBone as B, HumanoidPose, Pose, Skeleton, Tick, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_rig::full::FullRig;
use omuzaki_rig::ik::guard::{ArmGuard, BodyPart, BodyProxy, GuardConfig};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;

/// Counts this thread's allocations, so a frame can be shown to make none.
struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards to the system allocator; the counter is thread-local and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
        // SAFETY: the caller's contract is the system allocator's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The joints (left side and centre), metres.
fn joint(bone: B) -> Vec3 {
    let v = Vec3::new;
    match bone {
        B::Hips => v(0.0, 0.95, 0.0),
        B::Spine => v(0.0, 1.07, 0.0),
        B::Chest => v(0.0, 1.22, 0.0),
        B::UpperChest => v(0.0, 1.34, 0.0),
        B::Neck => v(0.0, 1.45, 0.0),
        B::Head => v(0.0, 1.53, 0.0),
        B::LeftShoulder => v(0.03, 1.42, 0.0),
        B::LeftUpperArm => v(0.19, 1.42, 0.0),
        B::LeftLowerArm => v(0.47, 1.42, 0.0),
        B::LeftHand => v(0.72, 1.42, 0.0),
        B::LeftUpperLeg => v(0.09, 0.9, 0.0),
        B::LeftLowerLeg => v(0.09, 0.48, 0.0),
        B::LeftFoot => v(0.09, 0.07, 0.0),
        _ => unreachable!("not in the bone list"),
    }
}

const BONES: [B; 20] = [
    B::Hips,
    B::Spine,
    B::Chest,
    B::UpperChest,
    B::Neck,
    B::Head,
    B::LeftShoulder,
    B::RightShoulder,
    B::LeftUpperArm,
    B::RightUpperArm,
    B::LeftLowerArm,
    B::RightLowerArm,
    B::LeftHand,
    B::RightHand,
    B::LeftUpperLeg,
    B::RightUpperLeg,
    B::LeftLowerLeg,
    B::RightLowerLeg,
    B::LeftFoot,
    B::RightFoot,
];

/// A joint on either side: a right bone is its left twin reflected.
fn position(bone: B) -> Vec3 {
    match bone.name().strip_prefix("right") {
        Some(rest) => {
            let p = joint(B::from_name(&format!("left{rest}")).unwrap());
            Vec3::new(-p.x, p.y, p.z)
        }
        None => joint(bone),
    }
}

fn skeleton() -> Skeleton {
    Skeleton::humanoid(&BONES, |bone| {
        let parent = std::iter::successors(bone.parent(), |p| p.parent()).find(|p| BONES.contains(p));
        Transform::from_translation(position(bone) - parent.map_or(Vec3::ZERO, position))
    })
    .unwrap()
}

/// The radii the synthetic mesh is built with.
const TORSO: f32 = 0.11;
const PELVIS: f32 = 0.1;
const THIGH: f32 = 0.07;
const HEAD: f32 = 0.11;
const FOREARM: f32 = 0.035;
const HAND: f32 = 0.045;
/// The head's centre, above the head joint.
const HEAD_CENTRE: Vec3 = Vec3::new(0.0, 1.63, 0.02);

/// Points on the surface of the capsule `a`-`b` of `radius`, each carried by `bone`.
fn capsule(a: Vec3, b: Vec3, radius: f32, bone: BoneId, out: &mut Vec<(Vec3, BoneId)>) {
    let axis = (b - a).try_normalize().unwrap_or(Vec3::Y);
    let (e1, e2) = axis.any_orthonormal_pair();
    for k in 0..=12 {
        let on = a.lerp(b, k as f32 / 12.0);
        for j in 0..24 {
            let u = j as f32 / 24.0 * std::f32::consts::TAU;
            out.push((on + (e1 * u.cos() + e2 * u.sin()) * radius, bone));
        }
    }
}

/// Points on a sphere, each carried by `bone`.
fn sphere(centre: Vec3, radius: f32, bone: BoneId, out: &mut Vec<(Vec3, BoneId)>) {
    for i in 0..12 {
        let polar = (i as f32 + 0.5) / 12.0 * std::f32::consts::PI;
        for j in 0..24 {
            let u = j as f32 / 24.0 * std::f32::consts::TAU;
            let dir = Vec3::new(polar.sin() * u.cos(), polar.cos(), polar.sin() * u.sin());
            out.push((centre + dir * radius, bone));
        }
    }
}

/// The synthetic mesh: torso, pelvis, thighs, head, forearms and hands at the radii above.
fn mesh(skeleton: &Skeleton) -> Vec<(Vec3, BoneId)> {
    let id = |bone: B| skeleton.humanoid_map().unwrap().get(bone).unwrap();
    let mut out = Vec::new();
    capsule(joint(B::Hips), joint(B::Chest), TORSO, id(B::Spine), &mut out);
    capsule(joint(B::Chest), joint(B::Neck), TORSO, id(B::Chest), &mut out);
    capsule(position(B::LeftUpperLeg), position(B::RightUpperLeg), PELVIS, id(B::Hips), &mut out);
    for (upper, lower) in [(B::LeftUpperLeg, B::LeftLowerLeg), (B::RightUpperLeg, B::RightLowerLeg)] {
        capsule(position(upper), position(lower), THIGH, id(upper), &mut out);
    }
    sphere(HEAD_CENTRE, HEAD, id(B::Head), &mut out);
    for (lower, hand, side) in [(B::LeftLowerArm, B::LeftHand, 1.0), (B::RightLowerArm, B::RightHand, -1.0)] {
        capsule(position(lower), position(hand), FOREARM, id(lower), &mut out);
        sphere(position(hand) + Vec3::X * side * 0.08, HAND, id(hand), &mut out);
    }
    out
}

/// The skeleton and its guard, fitted from the mesh.
fn guarded() -> (Skeleton, ArmGuard) {
    let skeleton = skeleton();
    let proxy = BodyProxy::fit(&skeleton, &skeleton.rest_pose(), &mesh(&skeleton)).unwrap();
    (skeleton, ArmGuard::new(proxy, GuardConfig::default()))
}

/// Both arms set from world directions of the upper arm and forearm (the left side's; the right mirrors it).
fn arms(skeleton: &Skeleton, upper: Vec3, fore: Vec3) -> Pose {
    let rig = HumanoidRig::new(RigSkeleton::new(skeleton.clone()));
    let mut pose = HumanoidPose::REST;
    for (bones, along, flip) in [
        ([B::LeftUpperArm, B::LeftLowerArm], Vec3::X, 1.0_f32),
        ([B::RightUpperArm, B::RightLowerArm], Vec3::NEG_X, -1.0),
    ] {
        let mirror = |v: Vec3| Vec3::new(v.x * flip, v.y, v.z);
        let u = Quat::from_rotation_arc(along, mirror(upper).normalize());
        let f = Quat::from_rotation_arc(along, mirror(fore).normalize());
        pose.set(bones[0], u);
        pose.set(bones[1], u.inverse() * f);
    }
    let mut out = Pose::IDENTITY;
    rig.apply_humanoid(&pose, &mut out);
    out
}

/// The poses that drive the arms into the body, with what they hit.
fn into_the_body(skeleton: &Skeleton) -> Vec<(&'static str, Pose)> {
    vec![
        // Elbows at the sides, forearms straight across through the chest.
        ("forearms through the chest", arms(skeleton, Vec3::new(0.15, -0.9, 0.35), Vec3::new(-1.0, 0.1, 0.05))),
        // Arms down, the hands swung in onto the thighs.
        ("hands into the thighs", arms(skeleton, Vec3::new(0.12, -1.0, 0.05), Vec3::new(-0.45, -0.88, 0.05))),
        // Hands brought up into the face.
        ("hands into the face", arms(skeleton, Vec3::new(0.15, -0.1, 0.98), Vec3::new(-0.55, 0.62, -0.56))),
    ]
}

fn frame_dt() -> f32 {
    1.0 / 60.0
}

#[test]
fn the_body_is_measured_from_its_mesh() {
    let skeleton = skeleton();
    let proxy = BodyProxy::fit(&skeleton, &skeleton.rest_pose(), &mesh(&skeleton)).unwrap();
    let radius = |part: BodyPart| proxy.body.iter().find(|p| p.part == part).unwrap().radius;
    for (part, want) in [
        (BodyPart::Torso, TORSO),
        (BodyPart::Pelvis, PELVIS),
        (BodyPart::LeftThigh, THIGH),
        (BodyPart::RightThigh, THIGH),
        (BodyPart::Head, HEAD),
    ] {
        let got = radius(part);
        println!("{part:?}: radius {:.1} mm (built at {:.1})", got * 1e3, want * 1e3);
        assert!((got - want).abs() < 0.003, "{part:?}: {got} vs {want}");
    }
    assert!((proxy.forearm_radius - FOREARM).abs() < 0.003, "{}", proxy.forearm_radius);
    for axes in proxy.hand_axes {
        assert!((axes - Vec3::splat(HAND)).abs().max_element() < 0.003, "{axes}");
    }
    let torso = proxy.body.iter().find(|p| p.part == BodyPart::Torso).unwrap();
    assert!((torso.depth - TORSO).abs() < 0.003, "a round torso is as deep as it is wide: {}", torso.depth);
    assert!((proxy.hands[0] - Vec3::X * 0.08).length() < 0.003 && (proxy.hands[1] + Vec3::X * 0.08).length() < 0.003);
    // No mesh on the torso or the forearms: nothing to measure.
    assert!(BodyProxy::fit(&skeleton, &skeleton.rest_pose(), &[]).is_none());
}

#[test]
fn arms_driven_into_the_body_end_the_frame_outside_it() {
    let (skeleton, mut guard) = guarded();
    for (name, posed) in into_the_body(&skeleton) {
        guard.reset();
        let mut pose = posed.clone();
        let report = guard.apply(&skeleton, &mut pose, frame_dt());
        println!(
            "{name}: deepest {:.1} mm before, {:.1} mm after, wrists moved {:.0} / {:.0} mm",
            report.before * 1e3,
            report.after * 1e3,
            report.moved[0] * 1e3,
            report.moved[1] * 1e3
        );
        assert!(report.before < -0.01, "{name}: the pose must really go into the body: {report:?}");
        assert!(report.after >= -GuardConfig::default().tolerance - 1e-4, "{name}: {report:?}");
        // Checked again from the outside: a second pass over the result finds nothing to do.
        let mut again = pose.clone();
        let check = ArmGuard::new(guard.proxy().clone(), GuardConfig::default()).apply(&skeleton, &mut again, 0.0);
        assert!(check.before >= -GuardConfig::default().tolerance - 1e-4, "{name}: {check:?}");
        // Only the arms moved.
        for (i, (a, b)) in posed.local.iter().zip(&pose.local).enumerate().take(skeleton.len()) {
            let arm = skeleton.humanoid_map().unwrap().iter().any(|(bone, id)| {
                id.index() == i
                    && matches!(
                        bone,
                        B::LeftUpperArm
                            | B::LeftLowerArm
                            | B::LeftHand
                            | B::RightUpperArm
                            | B::RightLowerArm
                            | B::RightHand
                    )
            });
            assert!(arm || a == b, "{name}: bone {i} moved");
        }
    }
}

#[test]
fn an_arm_that_is_clear_is_left_alone() {
    let (skeleton, mut guard) = guarded();
    let mut check = |posed: Pose| {
        let mut pose = posed.clone();
        let report = guard.apply(&skeleton, &mut pose, frame_dt());
        assert!(report.before > 0.0 && report.moved == [0.0; 2], "{report:?}");
        assert!(pose == posed, "a clear pose is untouched bit for bit");
    };
    check(skeleton.rest_pose());
    check(arms(&skeleton, Vec3::new(0.3, -0.95, 0.0), Vec3::new(0.3, -0.95, 0.1)));
}

#[test]
fn a_push_lets_go_smoothly_and_no_frame_shows_an_arm_inside() {
    let (skeleton, mut guard) = guarded();
    let clear = arms(&skeleton, Vec3::new(0.3, -0.95, 0.0), Vec3::new(0.3, -0.95, 0.1));
    let (_, into) = into_the_body(&skeleton).remove(1);
    let wrist = skeleton.humanoid_map().unwrap().get(B::LeftHand).unwrap();
    let at = |pose: &Pose| {
        let mut world = Pose::IDENTITY;
        skeleton.world_pose(pose, &mut world);
        world.local[wrist.index()].translation
    };
    let mut path = Vec::new();
    for frame in 0..90 {
        // Clear, then the hands swing into the thighs over a few frames and stay, then back out.
        let w = match frame {
            10..20 => (frame - 10) as f32 / 10.0,
            20..40 => 1.0,
            _ => 0.0,
        };
        let mut pose = clear.clone();
        for (out, (a, b)) in pose.local.iter_mut().zip(clear.local.iter().zip(&into.local)) {
            out.rotation = a.rotation.slerp(b.rotation, w);
        }
        let animated = at(&pose);
        let report = guard.apply(&skeleton, &mut pose, frame_dt());
        assert!(report.after >= -GuardConfig::default().tolerance - 1e-4, "frame {frame}: {report:?}");
        path.push((animated, at(&pose)));
    }
    // Once the animation leaves the body the push lets go: back on the animation exactly within half a second, and
    // never in a jump.
    let pushed = (path[39].1 - path[39].0).length();
    let back = (40..90).find(|&f| (path[f].1 - path[f].0).length() < 1e-6).unwrap();
    let steps: f32 = (41..90).map(|f| (path[f].1 - path[f - 1].1).length()).fold(0.0, f32::max);
    let animated_step = (path[40].0 - path[39].0).length();
    println!(
        "pushed {:.0} mm out; back on the animation {:.2} s after it left; largest step {:.1} mm (the animation's own: {:.1})",
        pushed * 1e3,
        (back - 40) as f32 / 60.0,
        steps * 1e3,
        animated_step * 1e3
    );
    assert!(pushed > 0.01 && back - 40 <= 30, "{pushed} {back}");
    assert!(steps < animated_step + 0.5 * pushed, "{steps}");
}

#[test]
fn a_frame_allocates_nothing_and_costs_microseconds() {
    let (skeleton, mut guard) = guarded();
    let poses: Vec<Pose> = into_the_body(&skeleton).into_iter().map(|(_, p)| p).collect();
    let mut pose = Pose::IDENTITY;
    for posed in &poses {
        pose.clone_from(posed);
        guard.apply(&skeleton, &mut pose, frame_dt());
    }
    let before = ALLOCATIONS.with(Cell::get);
    let mut best = std::time::Duration::MAX;
    for _ in 0..50 {
        for posed in &poses {
            pose.clone_from(posed);
            let began = std::time::Instant::now();
            std::hint::black_box(guard.apply(&skeleton, &mut pose, frame_dt()));
            best = best.min(began.elapsed());
        }
    }
    assert_eq!(ALLOCATIONS.with(Cell::get), before, "a guarded frame allocates");
    println!("guard, both arms in the body: best {best:?} per frame");
    // A ceiling, not this machine's number: the §10 pose budget is a millisecond.
    assert!(best < std::time::Duration::from_micros(200), "{best:?}");
}

#[test]
fn the_full_rig_runs_the_guard_last_before_the_springs() {
    let (skeleton, guard) = guarded();
    let rig_skeleton = RigSkeleton::new(skeleton.clone());
    let mut rig = FullRig::without_springs(Arc::clone(&rig_skeleton)).with_guard(guard);
    // The chest-crossing pose as a humanoid pose: the rig's own humanoid stage poses it, the guard keeps it out.
    let mut humanoid = HumanoidPose::REST;
    for (bones, along, flip) in [
        ([B::LeftUpperArm, B::LeftLowerArm], Vec3::X, 1.0_f32),
        ([B::RightUpperArm, B::RightLowerArm], Vec3::NEG_X, -1.0),
    ] {
        let mirror = |v: Vec3| Vec3::new(v.x * flip, v.y, v.z);
        let u = Quat::from_rotation_arc(along, mirror(Vec3::new(0.15, -0.9, 0.35)).normalize());
        let f = Quat::from_rotation_arc(along, mirror(Vec3::new(-1.0, 0.1, 0.05)).normalize());
        humanoid.set(bones[0], u);
        humanoid.set(bones[1], u.inverse() * f);
    }
    let mut out = Pose::IDENTITY;
    omuzaki_anim_api::Rig::solve(&mut rig, Tick::at_rate(0, 60.0), &humanoid, &mut out);
    let report = rig.guard_report();
    println!("full rig: {report:?}");
    assert!(report.before < -0.01 && report.after >= -GuardConfig::default().tolerance - 1e-4, "{report:?}");
}
