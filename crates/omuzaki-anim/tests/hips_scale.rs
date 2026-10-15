//! Hips translation scaling between characters of different height (#77): a walk made on a tall character, scaled to a
//! short one, keeps the feet on the floor.

#![allow(clippy::cast_precision_loss)]

use omuzaki_anim::poses::{HipsScaleError, hips_rest_height, retarget_hips, scale_hips};
use omuzaki_anim_api::{HumanoidBone, HumanoidPose, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};

/// A humanoid with every length multiplied by `size`: legs 0.95 m long and hips at 1.0 m for `size` 1.
fn character(size: f32) -> Skeleton {
    Skeleton::humanoid(&HumanoidBone::ALL, |bone| {
        let at = match bone {
            HumanoidBone::Hips => Vec3::new(0.0, 1.0, 0.0),
            HumanoidBone::LeftUpperLeg => Vec3::new(0.1, -0.05, 0.0),
            HumanoidBone::RightUpperLeg => Vec3::new(-0.1, -0.05, 0.0),
            HumanoidBone::LeftLowerLeg | HumanoidBone::RightLowerLeg => Vec3::new(0.0, -0.45, 0.0),
            HumanoidBone::LeftFoot | HumanoidBone::RightFoot => Vec3::new(0.0, -0.45, 0.0),
            HumanoidBone::LeftToes | HumanoidBone::RightToes => Vec3::new(0.0, -0.05, 0.1),
            _ => Vec3::new(0.0, 0.1, 0.0),
        };
        Transform::from_translation(at * size)
    })
    .expect("a complete humanoid")
}

/// The lowest world height of the feet and toes with `pose` on `skeleton`.
fn lowest_foot(skeleton: &Skeleton, pose: &HumanoidPose) -> f32 {
    let mut local = skeleton.rest_pose();
    for (i, bone) in skeleton.bones().iter().enumerate() {
        if let Some(h) = bone.humanoid {
            local.local[i].rotation = bone.rest.rotation * pose.get(h);
        }
    }
    let hips = skeleton.humanoid_bone(HumanoidBone::Hips).expect("hips");
    local.local[hips.index()].translation += pose.hips_offset;
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&local, &mut world);
    [HumanoidBone::LeftFoot, HumanoidBone::RightFoot, HumanoidBone::LeftToes, HumanoidBone::RightToes]
        .into_iter()
        .map(|b| world.local[skeleton.humanoid_bone(b).expect("foot").index()].translation.y)
        .fold(f32::INFINITY, f32::min)
}

/// A walk cycle on `tall`, with the hips lowered each frame so the lower foot rests on the floor.
fn walk(tall: &Skeleton, frames: usize) -> Vec<HumanoidPose> {
    (0..frames)
        .map(|i| {
            let phase = i as f32 / frames as f32 * std::f32::consts::TAU;
            let mut pose = HumanoidPose::REST;
            pose.set(HumanoidBone::LeftUpperLeg, Quat::from_rotation_x(0.45 * phase.sin()));
            pose.set(HumanoidBone::RightUpperLeg, Quat::from_rotation_x(-0.45 * phase.sin()));
            pose.set(HumanoidBone::LeftLowerLeg, Quat::from_rotation_x(-0.5 * phase.sin().max(0.0)));
            pose.set(HumanoidBone::RightLowerLeg, Quat::from_rotation_x(-0.5 * (-phase.sin()).max(0.0)));
            pose.hips_offset.z = 0.02 * i as f32;
            pose.hips_offset.y = -lowest_foot(tall, &pose);
            pose
        })
        .collect()
}

#[test]
fn a_tall_walk_scaled_to_a_short_character_keeps_its_feet_on_the_floor_within_a_millimetre() {
    let (tall, short) = (character(1.0), character(0.5));
    assert!((hips_rest_height(&tall).unwrap() - 1.0).abs() < 1e-6);
    assert!((hips_rest_height(&short).unwrap() - 0.5).abs() < 1e-6);
    let mut worst_scaled = 0.0_f32;
    let mut worst_unscaled = 0.0_f32;
    for pose in walk(&tall, 120) {
        assert!(lowest_foot(&tall, &pose).abs() < 1e-5, "the source walk stands on the floor");
        worst_unscaled = worst_unscaled.max(lowest_foot(&short, &pose).abs());
        let mut scaled = pose;
        retarget_hips(&mut scaled, &tall, &short).unwrap();
        worst_scaled = worst_scaled.max(lowest_foot(&short, &scaled).abs());
    }
    println!("feet-height error through the walk: unscaled {worst_unscaled:.5} m, scaled {worst_scaled:.6} m");
    assert!(worst_scaled < 1e-3, "scaled hips float or sink by {worst_scaled} m");
    assert!(
        worst_unscaled > 0.01,
        "the unscaled walk should miss the floor by well over a millimetre, got {worst_unscaled} m"
    );
}

#[test]
fn scaling_multiplies_the_whole_offset_and_leaves_rotations_alone() {
    let mut pose = HumanoidPose::REST;
    pose.hips_offset = Vec3::new(0.2, -0.4, 1.0);
    pose.set(HumanoidBone::Head, Quat::from_rotation_y(0.3));
    let before = pose;
    scale_hips(&mut pose, 1.0, 0.5).unwrap();
    assert_eq!(pose.hips_offset, Vec3::new(0.1, -0.2, 0.5));
    assert_eq!(pose.rotations, before.rotations);
    scale_hips(&mut pose, 0.5, 1.0).unwrap();
    assert!((pose.hips_offset - before.hips_offset).length() < 1e-6, "scaling back restores the offset");
}

#[test]
fn a_bad_height_is_refused_and_the_pose_is_untouched() {
    let mut pose = HumanoidPose::REST;
    pose.hips_offset = Vec3::ONE;
    for (from, to) in [(0.0, 1.0), (1.0, 0.0), (-1.0, 1.0), (f32::NAN, 1.0), (1.0, f32::INFINITY)] {
        assert!(matches!(scale_hips(&mut pose, from, to), Err(HipsScaleError::BadHeight { .. })), "{from} {to}");
        assert_eq!(pose.hips_offset, Vec3::ONE);
    }
}

#[test]
fn a_generic_skeleton_has_no_hips_height() {
    let generic = Skeleton::generic(vec![omuzaki_anim_api::BoneDef {
        name: "root".to_owned(),
        parent: None,
        rest: Transform::IDENTITY,
        humanoid: None,
    }])
    .unwrap();
    assert_eq!(hips_rest_height(&generic), Err(HipsScaleError::NotHumanoid));
}
