//! `prepare::tpose::normalise_rest`: an A-posed rest turns onto the VRM T-pose, the mesh keeps its sculpt, and the
//! authored rest comes back as the bind pose. Every scene is generated here.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{add_skinned_mesh, def, file_skinning, humanoid, max_error, runtime_skinning, world};
use omuzaki_anim_api::{HumanoidFrames, HumanoidPose, Pose, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::IoScene;
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_io_ops::prepare::tpose::normalise_rest;
use omuzaki_render_api::MtoonSupport;

/// The world position of the node called `name`.
fn at(scene: &IoScene, f: &common::Fixture, name: &str) -> Vec3 {
    world(scene)[f.node(name).index()].w_axis.truncate()
}

/// The direction from bone `a` to bone `b` in `scene`.
fn direction(scene: &IoScene, f: &common::Fixture, a: &str, b: &str) -> Vec3 {
    (at(scene, f, b) - at(scene, f, a)).normalize()
}

#[test]
fn an_a_pose_turns_onto_the_t_axes() {
    let f = humanoid(45.0);
    let mut scene = f.scene.clone();
    let rest = normalise_rest(&mut scene);
    for (side, x) in [("left", 1.0f32), ("right", -1.0)] {
        for (a, b) in [("UpperArm", "LowerArm"), ("LowerArm", "Hand"), ("Hand", "MiddleProximal")] {
            let d = direction(&scene, &f, &format!("{side}{a}"), &format!("{side}{b}"));
            assert!((d - Vec3::X * x).length() < 1e-5, "{side}{a} points {d}");
        }
        let leg = direction(&scene, &f, &format!("{side}UpperLeg"), &format!("{side}LowerLeg"));
        assert!((leg - Vec3::NEG_Y).length() < 1e-5, "{side} leg points {leg}");
    }
    assert_ne!(rest.bind, HumanoidPose::REST, "the authored A is the bind");
    assert!(rest.report.has_rule("avatar.rest_normalised"), "{:?}", rest.report);
}

#[test]
fn posing_the_t_skeleton_at_bind_gives_back_every_joint_and_the_skinning() {
    let mut f = humanoid(45.0);
    let joints: Vec<String> = f.nodes.keys().filter(|n| *n != "Root").cloned().collect();
    let joints: Vec<&str> = joints.iter().map(String::as_str).collect();
    add_skinned_mesh(&mut f, "Body", &joints, 0, None);
    let (avatar, _) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("an A-posed humanoid imports");
    assert_ne!(avatar.bind, HumanoidPose::REST);

    let rest: Vec<Transform> = avatar.skeleton.bones().iter().map(|b| b.rest).collect();
    let mut posed = Pose::IDENTITY;
    HumanoidFrames::new(&avatar.skeleton).apply_humanoid(&rest, &avatar.bind, &mut posed).expect("one per bone");
    let mut placed = Pose::IDENTITY;
    avatar.skeleton.world_pose(&posed, &mut placed);
    let original = world(&f.scene);
    let mut worst = 0.0f32;
    for (node, retained) in avatar.retained.nodes.iter().enumerate() {
        if let Some(bone) = retained.bone {
            worst = worst.max(placed.local[bone.index()].translation.distance(original[node].w_axis.truncate()));
        }
    }
    assert!(worst < 1e-6, "every joint at bind is where the file had it: {worst}");
    let error = max_error(&runtime_skinning(&avatar, &posed), &file_skinning(&f.scene));
    assert!(error < 1e-6, "the skinned mesh at bind is the file's: {error}");
}

#[test]
fn a_t_pose_is_left_alone() {
    let f = humanoid(0.0);
    let mut scene = f.scene.clone();
    let rest = normalise_rest(&mut scene);
    assert_eq!(scene, f.scene, "no rest changed");
    assert_eq!(rest.bind, HumanoidPose::REST);
    assert!(rest.report.findings.is_empty(), "{:?}", rest.report);
}

#[test]
fn small_corrections_are_silent_and_large_ones_reported() {
    let f = humanoid(5.0);
    let mut scene = f.scene.clone();
    let rest = normalise_rest(&mut scene);
    assert_ne!(scene, f.scene, "5 degrees is corrected");
    let d = direction(&scene, &f, "leftUpperArm", "leftLowerArm");
    assert!((d - Vec3::X).length() < 1e-5, "{d}");
    assert!(!rest.report.has_rule("avatar.rest_normalised"), "silently: {:?}", rest.report);

    let f = humanoid(0.5);
    let mut scene = f.scene.clone();
    normalise_rest(&mut scene);
    assert_eq!(scene, f.scene, "half a degree is within the 1 degree tolerance");

    let f = humanoid(15.0);
    let mut scene = f.scene.clone();
    assert!(normalise_rest(&mut scene).report.has_rule("avatar.rest_normalised"));
}

#[test]
fn a_thumb_off_its_axis_keeps_its_rest_and_is_reported() {
    let mut f = humanoid(0.0);
    let proximal = f.node("leftThumbProximal").index();
    let t = &mut f.scene.nodes[proximal].transform;
    t.translation = Quat::from_rotation_y(20f32.to_radians()) * t.translation;
    let mut scene = f.scene.clone();
    let rest = normalise_rest(&mut scene);
    assert_eq!(scene, f.scene, "the thumb keeps its authored rest, and nothing else needed turning");
    let thumb = rest.report.findings.iter().filter(|x| x.rule == "avatar.thumb_off_axis").collect::<Vec<_>>();
    assert_eq!(thumb.len(), 1, "{:?}", rest.report);
    assert!(thumb[0].message.contains("left"), "{}", thumb[0].message);
}

#[test]
fn two_runs_give_the_same_rest_and_bind() {
    let f = humanoid(40.0);
    let (mut a, mut b) = (f.scene.clone(), f.scene.clone());
    assert_eq!(normalise_rest(&mut a), normalise_rest(&mut b));
    assert_eq!(a, b);
}

/// A humanoid whose left shoulder (the upper arm's parent) is scaled by `scale` and turned by `turn` about Y, with the
/// upper arms 45 degrees down in their own frames.
fn under_scaled_shoulder(scale: Vec3, turn_degrees: f32) -> (common::Fixture, IoScene) {
    let mut f = humanoid(45.0);
    let shoulder = f.node("leftShoulder").index();
    f.scene.nodes[shoulder].transform.scale = scale;
    f.scene.nodes[shoulder].transform.rotation = Quat::from_rotation_y(turn_degrees.to_radians());
    let mut scene = f.scene.clone();
    normalise_rest(&mut scene);
    (f, scene)
}

#[test]
fn a_limb_under_a_non_uniformly_scaled_parent_still_lands_on_its_axis() {
    // (2, 1, 1) is the case a world-space swing gets wrong; the turned one shears it from the other side.
    for (scale, turn) in
        [(Vec3::new(2.0, 1.0, 1.0), 0.0), (Vec3::new(2.0, 1.0, 1.0), 30.0), (Vec3::new(1.0, 3.0, 0.5), 60.0)]
    {
        let (f, scene) = under_scaled_shoulder(scale, turn);
        for (a, b) in [("UpperArm", "LowerArm"), ("LowerArm", "Hand"), ("Hand", "MiddleProximal")] {
            let d = direction(&scene, &f, &format!("left{a}"), &format!("left{b}"));
            assert!((d - Vec3::X).length() < 1e-5, "left{a} under scale {scale} turned {turn} points {d}");
        }
        let right = direction(&scene, &f, "rightUpperArm", "rightLowerArm");
        assert!((right + Vec3::X).length() < 1e-5, "the unscaled side still points {right}");
    }
}

#[test]
fn a_correction_under_a_scaled_parent_changes_only_the_rotation() {
    let (f, scene) = under_scaled_shoulder(Vec3::new(2.0, 1.0, 1.0), 0.0);
    let upper = f.node("leftUpperArm").index();
    let (before, after) = (f.scene.nodes[upper].transform, scene.nodes[upper].transform);
    assert_eq!(before.scale, after.scale, "only the rotation changes");
    assert_eq!(before.translation, after.translation);
    assert!((after.rotation.length() - 1.0).abs() < 1e-6, "a unit rotation");
    assert!(before.rotation.angle_between(after.rotation) > 0.1, "the arm was turned");
}
