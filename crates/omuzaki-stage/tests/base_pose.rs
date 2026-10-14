//! The stage's base pose is the character's bind pose (decision 0014 §4): neutral display, layering of the life
//! layer and clips onto `bind`, blending of partial clips and weights, and a stick-figure sheet for the eye.
//!
//! Set `OMUZAKI_STAGE_PNG=/some/dir` to write the sheet (`stage-base-pose.png`) and one picture per panel.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    clippy::large_types_passed_by_value // test helpers take the 896-byte pose by value for readability
)]

use image::{Rgb, RgbImage};
use omuzaki_anim_api::skeleton::{HumanoidBone as B, HumanoidPose, Pose, Skeleton, Transform};
use omuzaki_anim_api::{Animator, BoneMask, Bus, Channel, PoseLayer, PoseLayers, PoseMode, Tick};
use omuzaki_anim_mock::animator::MockAnimator;
use omuzaki_char_api::CharacterRig;
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_stage::frame::pose::seed_base_pose;

const BONES: [B; 22] = [
    B::Hips,
    B::Spine,
    B::Chest,
    B::Neck,
    B::Head,
    B::LeftShoulder,
    B::LeftUpperArm,
    B::LeftLowerArm,
    B::LeftHand,
    B::RightShoulder,
    B::RightUpperArm,
    B::RightLowerArm,
    B::RightHand,
    B::LeftUpperLeg,
    B::LeftLowerLeg,
    B::LeftFoot,
    B::LeftToes,
    B::RightUpperLeg,
    B::RightLowerLeg,
    B::RightFoot,
    B::RightToes,
    B::UpperChest,
];

fn side(bone: B) -> f32 {
    if bone.name().starts_with("left") {
        1.0
    } else if bone.name().starts_with("right") {
        -1.0
    } else {
        0.0
    }
}

/// Offset from the parent in the T-pose (+X is the character's left).
fn t_offset(bone: B) -> Vec3 {
    let s = side(bone);
    match bone {
        B::Hips => Vec3::new(0.0, 0.95, 0.0),
        B::Spine | B::Chest | B::UpperChest | B::Neck => Vec3::new(0.0, 0.12, 0.0),
        B::Head => Vec3::new(0.0, 0.08, 0.0),
        B::LeftShoulder | B::RightShoulder => Vec3::new(s * 0.04, 0.06, 0.0),
        B::LeftUpperArm | B::RightUpperArm => Vec3::new(s * 0.10, 0.0, 0.0),
        B::LeftLowerArm | B::RightLowerArm => Vec3::new(s * 0.28, 0.0, 0.0),
        B::LeftHand | B::RightHand => Vec3::new(s * 0.26, 0.0, 0.0),
        B::LeftUpperLeg | B::RightUpperLeg => Vec3::new(s * 0.09, -0.05, 0.0),
        B::LeftLowerLeg | B::RightLowerLeg | B::LeftFoot | B::RightFoot => Vec3::new(0.0, -0.42, 0.0),
        _ => Vec3::new(0.0, -0.05, 0.1), // toes
    }
}

struct Fixture {
    rig: HumanoidRig,
    skeleton: Skeleton,
}

fn fixture() -> Fixture {
    let skeleton = Skeleton::humanoid(&BONES, |b| Transform::from_translation(t_offset(b))).unwrap();
    let rig = HumanoidRig::new(RigSkeleton::new(skeleton.clone()));
    Fixture { rig, skeleton }
}

/// The arms hang `degrees` from the T (an A-pose); `degrees = 0` is bound in the T.
fn a_bind(degrees: f32) -> HumanoidPose {
    let a = degrees.to_radians();
    let mut bind = HumanoidPose::REST;
    bind.set(B::LeftUpperArm, Quat::from_rotation_z(-a));
    bind.set(B::RightUpperArm, Quat::from_rotation_z(a));
    bind
}

/// Raises the arms (T space) and bends the elbows.
fn arms_up_clip() -> HumanoidPose {
    let mut p = HumanoidPose::REST;
    p.set(B::LeftUpperArm, Quat::from_rotation_z(0.5));
    p.set(B::RightUpperArm, Quat::from_rotation_z(-0.5));
    p.set(B::LeftLowerArm, Quat::from_rotation_z(0.9));
    p.set(B::RightLowerArm, Quat::from_rotation_z(-0.9));
    p.set(B::LeftUpperLeg, Quat::from_rotation_z(-0.3));
    p.set(B::RightUpperLeg, Quat::from_rotation_z(0.3));
    p
}

const FOREARMS: [B; 2] = [B::LeftLowerArm, B::RightLowerArm];
const ARMS: [B; 4] = [B::LeftUpperArm, B::RightUpperArm, B::LeftLowerArm, B::RightLowerArm];

/// A clip: an override layer in T space.
fn clip(pose: HumanoidPose, driven: BoneMask, weight: f32) -> PoseLayer {
    PoseLayer { pose, mask: driven, weight, mode: PoseMode::Override }
}

fn forearm_clip() -> PoseLayer {
    clip(arms_up_clip(), BoneMask::of(&FOREARMS), 1.0)
}

/// A life layer: an additive delta.
fn life_layer(pose: HumanoidPose, driven: BoneMask) -> PoseLayer {
    PoseLayer { pose, mask: driven, weight: 1.0, mode: PoseMode::Add }
}

/// A character record bound in `bind`.
fn rig_bound_in(bind: HumanoidPose, skeleton: &Skeleton) -> CharacterRig {
    CharacterRig {
        skeleton: skeleton.clone(),
        springs: omuzaki_anim_api::SpringSet::default(),
        drivers: Vec::new(),
        bind,
    }
}

/// World position of every bone for a humanoid pose.
fn world(f: &Fixture, pose: &HumanoidPose) -> Vec<Vec3> {
    let mut local = Pose::IDENTITY;
    f.rig.apply_humanoid(pose, &mut local);
    let mut world = Pose::IDENTITY;
    f.skeleton.world_pose(&local, &mut world);
    world.local[..f.skeleton.len()].iter().map(|t| t.translation).collect()
}

fn joint(f: &Fixture, w: &[Vec3], bone: B) -> Vec3 {
    w[f.skeleton.humanoid_bone(bone).unwrap().index()]
}

/// One frame as the frame loop runs it: seed `out` with the bind, run the animator on `bus` (the life layer's channels),
/// then the clips. `None` for the bus runs no animator at all.
fn assemble(f: &Fixture, bind: HumanoidPose, bus: Option<&Bus>, layers: &[PoseLayer]) -> HumanoidPose {
    let mut out = HumanoidPose::REST;
    seed_base_pose(&rig_bound_in(bind, &f.skeleton), &mut out);
    if let Some(bus) = bus {
        MockAnimator.animate(Tick::at_rate(0, 30.0), bus, &[], &mut out);
    }
    PoseLayers(layers).resolve(&mut out);
    out
}

/// The rotation between `a` and `b`, radians, from the quaternion difference (`acos` loses all precision near 0).
fn angle(a: Quat, b: Quat) -> f32 {
    (a - b).length().min((a + b).length()) * 2.0
}

fn assert_pose_close(a: &HumanoidPose, b: &HumanoidPose, tol: f32) {
    assert!(a.hips_offset.distance(b.hips_offset) <= tol);
    for bone in B::ALL {
        assert!(angle(a.get(bone), b.get(bone)) <= tol, "{bone}: {}", angle(a.get(bone), b.get(bone)));
    }
}

#[test]
fn no_animation_shows_the_bind_pose_not_the_t() {
    let f = fixture();
    let bind = a_bind(58.6);
    let shown = assemble(&f, bind, None, &[]);
    assert_eq!(shown, bind);
    // A quiet bus through the animator changes nothing either (breath at its neutral 0.5 is not exactly 0 tip).
    assert_pose_close(&assemble(&f, bind, Some(&Bus::default()), &[]), &bind, 0.01);

    // Joints: the shown pose puts the hands down by the sides, not out in the T.
    let (t, a) = (world(&f, &HumanoidPose::REST), world(&f, &shown));
    assert!(joint(&f, &a, B::LeftHand).y < joint(&f, &t, B::LeftHand).y - 0.3);

    // Skinning: with inverse bind matrices of the bind pose, the shown pose skins to the identity, so the mesh is the
    // mesh as built; the T rest would turn the arms.
    let mut bind_local = Pose::IDENTITY;
    f.rig.apply_humanoid(&bind, &mut bind_local);
    let inverse_bind = f.skeleton.inverse_bind_for(&bind_local);
    let skin = |pose: &HumanoidPose| -> Vec<Mat4> {
        let (mut local, mut scratch) = (Pose::IDENTITY, Pose::IDENTITY);
        f.rig.apply_humanoid(pose, &mut local);
        let mut out = vec![Mat4::IDENTITY; f.skeleton.len()];
        f.skeleton.skin_matrices(&local, &inverse_bind, &mut scratch, &mut out);
        out
    };
    for m in skin(&shown) {
        assert!(m.abs_diff_eq(Mat4::IDENTITY, 1e-5), "{m:?}");
    }
    assert!(skin(&HumanoidPose::REST).iter().any(|m| !m.abs_diff_eq(Mat4::IDENTITY, 1e-2)));
}

#[test]
fn a_forearm_only_clip_leaves_the_upper_arms_in_the_a_bind() {
    let f = fixture();
    let bind = a_bind(58.6);
    let shown = assemble(&f, bind, None, &[forearm_clip()]);

    for (upper, lower) in [(B::LeftUpperArm, B::LeftLowerArm), (B::RightUpperArm, B::RightLowerArm)] {
        assert_eq!(shown.get(upper), bind.get(upper), "{upper} stays in the A bind");
        assert_ne!(shown.get(upper), Quat::IDENTITY, "{upper} is not the T rest");
        assert_eq!(shown.get(lower), arms_up_clip().get(lower), "{lower} is driven in T space");
    }
    // The elbow is where the bind puts it: nothing above the forearm moved.
    let (shown_w, bind_w) = (world(&f, &shown), world(&f, &bind));
    assert!(joint(&f, &shown_w, B::LeftLowerArm).distance(joint(&f, &bind_w, B::LeftLowerArm)) < 1e-6);
    // Bones outside the mask are the bind's, legs included.
    assert_eq!(shown.get(B::LeftUpperLeg), bind.get(B::LeftUpperLeg));
}

#[test]
fn a_full_body_clip_is_the_clip() {
    let f = fixture();
    let mut pose = arms_up_clip();
    pose.hips_offset = Vec3::new(0.0, -0.1, 0.05);
    let shown = assemble(&f, a_bind(58.6), None, &[clip(pose, BoneMask::ALL, 1.0)]);
    assert_eq!(shown, pose);
}

#[test]
fn half_weight_blends_from_the_bind_not_from_the_t() {
    let f = fixture();
    let bind = a_bind(58.6);
    let pose = arms_up_clip();
    let layer = clip(pose, BoneMask::of(&ARMS), 0.5);
    let shown = assemble(&f, bind, None, &[layer]);
    for bone in ARMS {
        let expected = bind.get(bone).slerp(pose.get(bone), 0.5);
        assert!(angle(shown.get(bone), expected) < 1e-6, "{bone}");
    }
    // Upper arm: equally far from the bind and from the clip; blending from the T would not be.
    let up = B::LeftUpperArm;
    let (to_bind, to_clip) = (angle(shown.get(up), bind.get(up)), angle(shown.get(up), pose.get(up)));
    assert!((to_bind - to_clip).abs() < 1e-5);
    let from_t = Quat::IDENTITY.slerp(pose.get(up), 0.5);
    assert!(angle(shown.get(up), from_t) > 0.3);
    // Weight 0 is the bind; a NaN weight is ignored rather than poisoning the pose.
    for w in [0.0, -1.0, f32::NAN] {
        assert_eq!(assemble(&f, bind, None, &[PoseLayer { weight: w, ..layer }]), bind);
    }
}

#[test]
fn the_life_layer_adds_onto_the_bind() {
    let f = fixture();
    let bind = a_bind(58.6);
    // The animator's life channels: lean bends the spine, which the bind has already bent.
    let mut bind_bent = bind;
    bind_bent.set(B::Spine, Quat::from_rotation_x(0.1));
    let mut bus = Bus::default();
    bus.values[Channel::LEAN] = 0.2;
    let shown = assemble(&f, bind_bent, Some(&bus), &[]);
    assert!(angle(shown.get(B::Spine), Quat::from_rotation_x(0.3)) < 1e-5, "lean adds to the bound spine");
    assert_eq!(shown.get(B::LeftUpperArm), bind.get(B::LeftUpperArm), "an undriven arm stays in the bind");

    // A sway layer on an arm the bind has moved: the same sway turns the arm by the same angle in the world whatever
    // the bind (A or a steeper A), because it is a delta in the parent's T space.
    let mut sway = HumanoidPose::REST;
    sway.set(B::LeftUpperArm, Quat::from_rotation_z(0.05));
    let sway = life_layer(sway, BoneMask::of(&[B::LeftUpperArm]));
    let swing = |degrees: f32| {
        let bind = a_bind(degrees);
        let (quiet, swayed) =
            (world(&f, &assemble(&f, bind, None, &[])), world(&f, &assemble(&f, bind, None, &[sway])));
        let dir = |w: &[Vec3]| (joint(&f, w, B::LeftLowerArm) - joint(&f, w, B::LeftUpperArm)).normalize();
        dir(&quiet).angle_between(dir(&swayed))
    };
    assert!((swing(58.6) - 0.05).abs() < 1e-4 && (swing(35.0) - 0.05).abs() < 1e-4);

    // A clip on top replaces the life layer's value on the bones it drives.
    let on_top = clip(arms_up_clip(), BoneMask::of(&[B::LeftUpperArm]), 1.0);
    let shown = assemble(&f, bind, Some(&bus), &[sway, on_top]);
    assert_eq!(shown.get(B::LeftUpperArm), arms_up_clip().get(B::LeftUpperArm));
}

#[test]
fn a_clip_gives_the_same_world_arm_on_characters_with_different_binds() {
    let f = fixture();
    let layer = clip(arms_up_clip(), BoneMask::ALL, 1.0);
    let dir = |degrees: f32| {
        let w = world(&f, &assemble(&f, a_bind(degrees), None, &[layer]));
        (joint(&f, &w, B::LeftLowerArm) - joint(&f, &w, B::LeftUpperArm)).normalize()
    };
    assert!(dir(58.6).distance(dir(20.0)) < 1e-6);
    assert!(dir(0.0).distance(dir(35.0)) < 1e-6);
}

// ---- the stick-figure sheet -------------------------------------------------------------------------------------

const PANEL: (u32, u32) = (320, 440);
const SCALE: f32 = 190.0; // pixels per metre

fn line(img: &mut RgbImage, a: (f32, f32), b: (f32, f32), colour: Rgb<u8>, width: i32) {
    let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) as i32).max(1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let (x, y) = ((a.0 + (b.0 - a.0) * t) as i32, (a.1 + (b.1 - a.1) * t) as i32);
        for dx in -width / 2..=width / 2 {
            for dy in -width / 2..=width / 2 {
                let (px, py) = (x + dx, y + dy);
                if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                    img.put_pixel(px as u32, py as u32, colour);
                }
            }
        }
    }
}

fn draw(img: &mut RgbImage, f: &Fixture, pose: &HumanoidPose, driven: BoneMask, colour: Rgb<u8>, width: i32) {
    let w = world(f, pose);
    let at = |p: Vec3| (PANEL.0 as f32 / 2.0 + p.x * SCALE, PANEL.1 as f32 - 25.0 - p.y * SCALE);
    for bone in BONES {
        let Some(parent_bone) = bone.parent() else { continue };
        let parent = f.skeleton.humanoid_bone(parent_bone).unwrap();
        let child = f.skeleton.humanoid_bone(bone).unwrap();
        // A segment is the parent bone (it spans parent joint to child joint), so it is red when the parent is driven.
        let hot = if driven.contains(parent_bone) { Rgb([220, 40, 40]) } else { colour };
        line(img, at(w[parent.index()]), at(w[child.index()]), hot, width);
    }
}

#[test]
fn stick_figure_sheet() {
    let f = fixture();
    let bind = a_bind(58.6);
    let all = clip(arms_up_clip(), BoneMask::ALL, 1.0);
    let half = clip(arms_up_clip(), BoneMask::of(&ARMS), 0.5);
    let panels: [(&str, Vec<PoseLayer>); 4] = [
        ("1-neutral", vec![]),
        ("2-forearm-only", vec![forearm_clip()]),
        ("3-full-clip", vec![all]),
        ("4-half-weight", vec![half]),
    ];
    let mut sheet = RgbImage::from_pixel(PANEL.0 * 4, PANEL.1, Rgb([248, 248, 246]));
    for (i, (name, clips)) in panels.iter().enumerate() {
        let mut img = RgbImage::from_pixel(PANEL.0, PANEL.1, Rgb([248, 248, 246]));
        // Ghosts: the T rest (pale blue), the bind (grey). The figure on top is black, driven bones red.
        draw(&mut img, &f, &HumanoidPose::REST, BoneMask::EMPTY, Rgb([190, 210, 240]), 3);
        draw(&mut img, &f, &bind, BoneMask::EMPTY, Rgb([170, 170, 170]), 3);
        let shown = assemble(&f, bind, None, clips);
        let driven = clips.first().map_or(BoneMask::EMPTY, |c| c.mask);
        draw(&mut img, &f, &shown, driven, Rgb([20, 20, 20]), 5);
        image::imageops::replace(&mut sheet, &img, i64::from(PANEL.0) * i as i64, 0);
        if let Ok(dir) = std::env::var("OMUZAKI_STAGE_PNG") {
            img.save(format!("{dir}/stage-base-pose-{name}.png")).unwrap();
        }
    }
    if let Ok(dir) = std::env::var("OMUZAKI_STAGE_PNG") {
        sheet.save(format!("{dir}/stage-base-pose.png")).unwrap();
    }
}
