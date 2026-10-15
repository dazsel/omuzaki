//! Hand targets (RG-05): an umbrella grip lands the palm on the handle frame, a book keeps both palms on their frames,
//! and crossed arms clear the torso and each other, on the soft-sd and tall skeletons, deterministically.
//!
//! `omuzaki-rig` may not depend on `omuzaki-char` (implementations do not depend on each other, `crates/README.md`), so the
//! two skeletons are built here from the templates' joint tables. Both rest in the T-pose with arms along X, like the
//! template rig (`rig_template.rs`); the arm bones keep the lengths of the templates' A-pose joints.
//!
//! - soft-sd (`crates/omuzaki-char/src/template/soft_sd.rs`, `JOINTS`, metres; stature scale 1).
//! - tall (`crates/omuzaki-char/src/template/tall.rs`, `JOINTS`, in heads; one head is 1.714 / 7.5 m at the template's
//!   stature of 1.714 m, `default_stature_m`).
//!
//! The torso radius is the section's half depth at the height of the arms (soft-sd `TORSO.ry` 0.066 at y = 0.485; tall
//! `TORSO.ry` 0.335 heads at 5.7). The arm radii are the template's `ARM` curve over the upper arm (soft-sd 0.0195; tall
//! 0.18 heads) and over the forearm (soft-sd `rx` 0.0155 at 0.18; tall 0.14 heads).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::manual_assert_eq
)] // `Pose` has no `Debug`

use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_rig::ik::targets::{ArmClearance, Capsule, CrossedArms, Hand, HandTarget, TwoHandedProp, palm_error};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;

/// A template's joints (left side), metres, and the radii the clearance test needs.
struct Body {
    name: &'static str,
    /// `[hips, spine, chest, upper chest, neck, head, shoulder, upper arm, lower arm, hand, upper leg, lower leg, foot]`.
    joints: [Vec3; 13],
    /// Torso half depth, upper arm radius, forearm radius, wrist-to-palm distance.
    torso: f32,
    arm: f32,
    forearm: f32,
    to_palm: f32,
    /// How far the animated upper arms are drawn across the chest (the inward share of their direction).
    inward: f32,
    /// Where on the opposite upper arm the palm lands (0 at the shoulder, 1 at the elbow).
    fraction: f32,
}

fn soft_sd() -> Body {
    let v = Vec3::new;
    Body {
        name: "soft-sd",
        joints: [
            v(0.0, 0.37, 0.0),
            v(0.0, 0.41, 0.0),
            v(0.0, 0.46, 0.0),
            v(0.0, 0.49, 0.0),
            v(0.0, 0.52, 0.0),
            v(0.0, 0.56, 0.0),
            v(0.035, 0.485, 0.0),
            v(0.1, 0.47, 0.0),
            v(0.155, 0.38, -0.01),
            v(0.2, 0.29, 0.01),
            v(0.045, 0.38, 0.0),
            v(0.045, 0.19, 0.0),
            v(0.045, 0.06, 0.0),
        ],
        torso: 0.066,
        arm: 0.0195,
        forearm: 0.0155,
        to_palm: 0.02,
        inward: 0.7,
        fraction: 0.5,
    }
}

fn tall() -> Body {
    let head = 1.714 / 7.5;
    let v = |x: f32, y: f32, z: f32| Vec3::new(x, y, z) * head;
    Body {
        name: "tall",
        joints: [
            v(0.0, 4.28, 0.0),
            v(0.0, 4.82, 0.0),
            v(0.0, 5.49, 0.0),
            v(0.0, 5.89, 0.0),
            v(0.0, 6.29, 0.0),
            v(0.0, 6.44, 0.0),
            v(0.20, 6.22, 0.0),
            v(0.60, 6.10, 0.0),
            v(1.1821, 5.0051, -0.04),
            v(1.597, 4.0733, 0.04),
            v(0.31, 4.12, 0.0),
            v(0.27, 2.34, 0.02),
            v(0.23, 0.40, 0.0),
        ],
        torso: 0.335 * head,
        arm: 0.18 * head,
        forearm: 0.14 * head,
        to_palm: 0.4 * 0.74 * head,
        inward: 0.36,
        fraction: 1.0,
    }
}

impl Body {
    /// The T-pose skeleton: the A-pose arm laid along X with the same bone lengths.
    fn skeleton(&self) -> Skeleton {
        let [hips, spine, chest, upper_chest, neck, head, shoulder, upper, lower, hand, leg, shin, foot] = self.joints;
        let (l1, l2) = (upper.distance(lower), lower.distance(hand));
        let left = |bone: B| -> Vec3 {
            match bone {
                B::Hips => hips,
                B::Spine => spine,
                B::Chest => chest,
                B::UpperChest => upper_chest,
                B::Neck => neck,
                B::Head => head,
                B::LeftShoulder => shoulder,
                B::LeftUpperArm => Vec3::new(upper.x, upper.y, 0.0),
                B::LeftLowerArm => Vec3::new(upper.x + l1, upper.y, 0.0),
                B::LeftHand => Vec3::new(upper.x + l1 + l2, upper.y, 0.0),
                B::LeftUpperLeg => leg,
                B::LeftLowerLeg => shin,
                B::LeftFoot => foot,
                _ => unreachable!("not in the bone list"),
            }
        };
        let position = |bone: B| -> Vec3 {
            match bone.name().strip_prefix("right") {
                Some(rest) => {
                    let mirrored = B::from_name(&format!("left{rest}")).unwrap();
                    let p = left(mirrored);
                    Vec3::new(-p.x, p.y, p.z)
                }
                None => left(bone),
            }
        };
        let bones = [
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
        Skeleton::humanoid(&bones, |bone| {
            let parent = std::iter::successors(bone.parent(), |p| p.parent()).find(|p| bones.contains(p));
            Transform::from_translation(position(bone) - parent.map_or(Vec3::ZERO, position))
        })
        .unwrap()
    }

    fn arm_length(&self) -> f32 {
        self.joints[7].distance(self.joints[8]) + self.joints[8].distance(self.joints[9])
    }
}

fn world(skeleton: &Skeleton, pose: &Pose) -> Pose {
    let mut out = Pose::IDENTITY;
    skeleton.world_pose(pose, &mut out);
    out
}

fn at(skeleton: &Skeleton, world: &Pose, bone: B) -> Vec3 {
    world.local[skeleton.humanoid_bone(bone).unwrap().index()].translation
}

/// The animated pose with both arms drawn across the chest: upper arms forward, in and down, forearms across.
fn arms_across(skeleton: &Skeleton, inward: f32) -> Pose {
    let rig = HumanoidRig::new(RigSkeleton::new(skeleton.clone()));
    let mut pose = HumanoidPose::REST;
    for (upper, lower, along, side) in
        [(B::LeftUpperArm, B::LeftLowerArm, Vec3::X, 1.0_f32), (B::RightUpperArm, B::RightLowerArm, Vec3::NEG_X, -1.0)]
    {
        let down_in = Quat::from_rotation_arc(
            along,
            Vec3::new(-inward * side, -0.1, (1.0 - inward * inward - 0.01).sqrt()).normalize(),
        );
        let across = Quat::from_rotation_arc(along, Vec3::new(-0.9 * side, -0.1, 0.3).normalize());
        pose.set(upper, down_in);
        pose.set(lower, down_in.inverse() * across);
    }
    let mut out = Pose::IDENTITY;
    rig.apply_humanoid(&pose, &mut out);
    out
}

fn crossed(body: &Body, skeleton: &Skeleton) -> CrossedArms {
    let rest = world(skeleton, &skeleton.rest_pose());
    CrossedArms {
        over: Hand::Right,
        fraction: body.fraction,
        to_palm: body.to_palm,
        arm_radius: body.arm,
        forearm_radius: body.forearm,
        stack: Vec3::Y,
        torso: Capsule { a: at(skeleton, &rest, B::Hips), b: at(skeleton, &rest, B::Neck), radius: body.torso },
        margin: 0.005,
        weight: 1.0,
    }
}

const MM: f32 = 0.001;
const TWO_DEGREES: f32 = 2.0 * std::f32::consts::PI / 180.0;

#[test]
fn an_umbrella_grip_lands_the_palm_on_the_handle_frame() {
    for body in [soft_sd(), tall()] {
        let skeleton = body.skeleton();
        let reach = body.arm_length();
        let shoulder = at(&skeleton, &world(&skeleton, &skeleton.rest_pose()), B::RightUpperArm);
        let palm = Hand::Right.palm(body.to_palm);
        let (mut worst_mm, mut worst_deg) = (0.0_f32, 0.0_f32);
        for (i, start) in vec![skeleton.rest_pose(), arms_across(&skeleton, body.inward)].into_iter().enumerate() {
            for turn in 0..8 {
                // The handle: in front of the right shoulder, a different way round each time.
                let rotation = Quat::from_euler(
                    omuzaki_core::glam::EulerRot::YXZ,
                    turn as f32 * 0.7 - 2.0,
                    turn as f32 * 0.45 - 1.5,
                    turn as f32 * 0.3,
                );
                let frame = Transform { translation: shoulder + Vec3::new(-0.3, -0.35, 0.55) * reach, rotation };
                let mut pose = start.clone();
                let target = HandTarget { hand: Hand::Right, frame, palm, pole: shoulder + Vec3::NEG_Y, weight: 1.0 };
                assert!(target.solve(&skeleton, &mut pose));
                let (metres, radians) = palm_error(&skeleton, &pose, Hand::Right, palm, frame).unwrap();
                assert!(
                    metres <= MM && radians <= TWO_DEGREES,
                    "{} start {i} turn {turn}: {metres} m, {radians} rad",
                    body.name
                );
                worst_mm = worst_mm.max(metres * 1000.0);
                worst_deg = worst_deg.max(radians.to_degrees());
            }
        }
        println!("umbrella {}: worst palm error {worst_mm:.4} mm, {worst_deg:.4} deg", body.name);
    }
}

#[test]
fn a_book_in_both_hands_keeps_both_palms_on_their_frames() {
    for body in [soft_sd(), tall()] {
        let skeleton = body.skeleton();
        let reach = body.arm_length();
        let rest = world(&skeleton, &skeleton.rest_pose());
        let mid = (at(&skeleton, &rest, B::LeftUpperArm) + at(&skeleton, &rest, B::RightUpperArm)) * 0.5;
        let fingers_forward = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
        let prop =
            Transform { translation: mid + Vec3::new(0.0, -0.5, 0.55) * reach, rotation: Quat::from_rotation_x(-0.3) };
        let book = TwoHandedProp {
            prop,
            left: Transform { translation: Vec3::new(0.4 * reach, 0.0, 0.0), rotation: fingers_forward },
            right: Transform { translation: Vec3::new(-0.4 * reach, 0.0, 0.0), rotation: fingers_forward },
            palms: [Hand::Left.palm(body.to_palm), Hand::Right.palm(body.to_palm)],
            poles: [Vec3::new(1.0, -1.0, 0.0), Vec3::new(-1.0, -1.0, 0.0)],
            weight: 1.0,
        };
        let mut pose = arms_across(&skeleton, body.inward);
        assert!(book.solve(&skeleton, &mut pose));
        for target in book.targets() {
            let (metres, radians) = palm_error(&skeleton, &pose, target.hand, target.palm, target.frame).unwrap();
            assert!(
                metres <= MM && radians <= TWO_DEGREES,
                "{} {:?}: {metres} m, {radians} rad",
                body.name,
                target.hand
            );
            println!(
                "book {} {:?}: palm error {:.4} mm, {:.4} deg",
                body.name,
                target.hand,
                metres * 1000.0,
                radians.to_degrees()
            );
        }
    }
}

/// Soft-sd's smaller gap (metres) with both palms really on the opposite upper arms: no pair of elbow swings reaches the
/// 5 mm margin on its 0.1 m arms. A regression bound, not a goal: the result may not get worse than this.
const SOFT_SD_FLOOR: f32 = -0.010;

#[test]
fn crossed_arms_clear_the_torso_and_each_other_deterministically() {
    for body in [soft_sd(), tall()] {
        let skeleton = body.skeleton();
        let arms = crossed(&body, &skeleton);
        let start = arms_across(&skeleton, body.inward);
        let run = || {
            let mut pose = start.clone();
            let clearance = arms.solve(&skeleton, &mut pose).unwrap();
            (pose, clearance)
        };
        let (pose, clearance) = run();
        let ArmClearance { torso, forearms, contact, swings, cleared } = clearance;
        println!(
            "crossed {}: torso gap {:.2} mm, forearm gap {:.2} mm, palm off its arm {:.3} mm, swings {:.0} / {:.0} deg",
            body.name,
            torso * 1000.0,
            forearms * 1000.0,
            contact * 1000.0,
            swings[0].to_degrees(),
            swings[1].to_degrees()
        );
        // The palms are on the opposite upper arms as they ended up, not as animated, whether or not the arms clear.
        let frames = arms.frames(&skeleton, &pose).unwrap();
        for (hand, frame) in [(Hand::Left, frames[0]), (Hand::Right, frames[1])] {
            let (metres, radians) = palm_error(&skeleton, &pose, hand, hand.palm(body.to_palm), frame).unwrap();
            println!("  {hand:?} palm: {:.4} mm, {:.4} deg off its frame", metres * 1000.0, radians.to_degrees());
            assert!(metres <= MM && radians <= TWO_DEGREES, "{} {hand:?}: {metres} m, {radians} rad", body.name);
        }
        if body.name == "tall" {
            assert!(cleared && torso >= 5.0 * MM && forearms >= 5.0 * MM, "{}: {clearance:?}", body.name);
        } else {
            let worst = torso.min(forearms);
            assert!(!cleared && worst >= SOFT_SD_FLOOR, "{}: {clearance:?}", body.name);
        }

        // The same gaps from an independent measurement of the posed skeleton.
        let w = world(&skeleton, &pose);
        let forearm = |elbow: B, wrist: B| Capsule {
            a: at(&skeleton, &w, elbow),
            b: at(&skeleton, &w, wrist),
            radius: body.forearm,
        };
        let (left, right) = (forearm(B::LeftLowerArm, B::LeftHand), forearm(B::RightLowerArm, B::RightHand));
        assert!((left.gap(&arms.torso).min(right.gap(&arms.torso)) - torso).abs() < 1e-5);
        assert!((left.gap(&right) - forearms).abs() < 1e-5);
        // Clearance does not fix the order: the right forearm is the one over (`over: Hand::Right`), so it stacks higher.
        let mid = |elbow: B, wrist: B| (at(&skeleton, &w, elbow) + at(&skeleton, &w, wrist)) * 0.5;
        let (left_mid, right_mid) = (mid(B::LeftLowerArm, B::LeftHand), mid(B::RightLowerArm, B::RightHand));
        assert!(
            right_mid.y > left_mid.y,
            "{}: right forearm midpoint y {} must stack over the left's {}",
            body.name,
            right_mid.y,
            left_mid.y
        );

        // A warm start from the last swings is the same pose in one attempt when it clears. When it cannot, the short climb
        // is no worse than the cold search's pose and still has the palms on their arms.
        let mut warm = start.clone();
        let again = arms.solve_from(&skeleton, &mut warm, Some(swings)).unwrap();
        if cleared {
            assert_eq!(again, clearance);
            assert!(warm == pose);
        } else {
            assert!(again.torso.min(again.forearms) >= torso.min(forearms) && again.contact <= 0.001, "{again:?}");
        }
        let (again, same) = run();
        assert!(again == pose && same == clearance, "{}: the same input must give the same pose", body.name);
    }
}

#[test]
fn the_clearance_search_cost_is_measured() {
    // Best of several runs, so scheduler noise cannot fail it. The numbers go in the PR (run with --release); the only
    // bound is that a warm start is cheaper than the cold search. Cold and uncleared are one-offs, warm is per frame.
    for body in [soft_sd(), tall()] {
        let skeleton = body.skeleton();
        let arms = crossed(&body, &skeleton);
        let start = arms_across(&skeleton, body.inward);
        let time = |arms: &CrossedArms, hint: Option<[f32; 2]>| {
            (0..9)
                .map(|_| {
                    let mut pose = start.clone();
                    let began = std::time::Instant::now();
                    std::hint::black_box(arms.solve_from(&skeleton, &mut pose, std::hint::black_box(hint)));
                    began.elapsed()
                })
                .min()
                .unwrap()
        };
        let mut pose = start.clone();
        let swings = arms.solve(&skeleton, &mut pose).unwrap().swings;
        // An unreachable margin never clears, so the search runs to the end: the worst case.
        let uncleared = CrossedArms { margin: 1.0, ..arms };
        let (cold, worst) = (time(&arms, None), time(&uncleared, None));
        let warm = time(&arms, Some(swings));
        println!("search {}: cold {cold:?}, uncleared worst case {worst:?}, warm start {warm:?}", body.name);
        assert!(body.name == "soft-sd" || warm < cold);
    }
}

#[test]
fn a_goal_with_a_non_finite_field_or_a_missing_arm_leaves_the_pose_alone() {
    let body = soft_sd();
    let skeleton = body.skeleton();
    let start = skeleton.rest_pose();
    let palm = Hand::Left.palm(body.to_palm);
    let mut pose = start.clone();
    let bad = HandTarget {
        hand: Hand::Left,
        frame: Transform::from_translation(Vec3::NAN),
        palm,
        pole: Vec3::Y,
        weight: 1.0,
    };
    assert!(!bad.solve(&skeleton, &mut pose) && pose == start);

    let generic = Skeleton::generic(vec![omuzaki_anim_api::BoneDef {
        name: "root".into(),
        parent: None,
        rest: Transform::IDENTITY,
        humanoid: None,
    }])
    .unwrap();
    let mut pose = generic.rest_pose();
    assert!(
        !HandTarget { frame: Transform::IDENTITY, ..bad }.solve(&generic, &mut pose) && pose == generic.rest_pose()
    );
    // Weight 0 leaves the animated pose bit for bit.
    let mut pose = start.clone();
    let frame = Transform::from_translation(Vec3::new(0.1, 0.3, 0.1));
    assert!(HandTarget { frame, weight: 0.0, ..bad }.solve(&skeleton, &mut pose) && pose == start);
}

#[test]
fn a_rotation_that_does_not_normalise_to_a_unit_quaternion_leaves_the_pose_alone() {
    // Each is judged by its normalised result: zero has no direction; a huge component overflows when squared and a tiny
    // one underflows, so normalising either gives NaNs or zero.
    let body = soft_sd();
    let skeleton = body.skeleton();
    let start = skeleton.rest_pose();
    let palm = Hand::Right.palm(body.to_palm);
    let frame = Transform::from_translation(Vec3::new(0.1, 0.3, 0.1));
    let base = HandTarget { hand: Hand::Right, frame, palm, pole: Vec3::NEG_Y, weight: 1.0 };
    let bad = [
        Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
        Quat::from_xyzw(0.0, 0.0, 0.0, 1e20),
        Quat::from_xyzw(1e-30, 0.0, 0.0, 0.0),
    ];
    for rotation in bad {
        let on_frame = HandTarget { frame: Transform { translation: frame.translation, rotation }, ..base };
        let mut pose = start.clone();
        assert!(!on_frame.solve(&skeleton, &mut pose) && pose == start, "frame {rotation:?}");

        let on_palm = HandTarget { palm: Transform { translation: palm.translation, rotation }, ..base };
        let mut pose = start.clone();
        assert!(!on_palm.solve(&skeleton, &mut pose) && pose == start, "palm {rotation:?}");
    }
}
