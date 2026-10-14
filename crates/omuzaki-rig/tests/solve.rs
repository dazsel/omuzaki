//! The humanoid solve and the skinning matrices against expectations computed without the rig's code.
//!
//! `common::expected_world` walks the hierarchy the way VRM defines normalised motion. A rig is right when its forward
//! kinematics lands every bone there, whatever the bones' own rest axes are, and when the skinning matrices carry any
//! point rigidly attached to a bone along with it.

mod common;

use common::{Character, character, expected_world, full_character, random_pose, same_rotation};
use omuzaki_anim_api::{HumanoidBone, HumanoidPose, Pose};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

const EPS: f32 = 1e-4;

fn rig_for(ch: &Character) -> HumanoidRig {
    HumanoidRig::new(RigSkeleton::new(ch.skeleton.clone()))
}

fn world_of(ch: &Character, local: &Pose) -> Pose {
    let mut world = Pose::IDENTITY;
    ch.skeleton.world_pose(local, &mut world);
    world
}

/// Every character variant: aligned and tilted axes, full and required-only bone sets.
fn variants() -> Vec<(&'static str, Character)> {
    let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
    vec![
        ("aligned, 128 joints", full_character(false)),
        ("tilted, 128 joints", full_character(true)),
        ("aligned, required bones only", character(&required, false, 2, 3, 3)),
        ("tilted, required bones only", character(&required, true, 2, 3, 4)),
    ]
}

#[test]
fn solved_bones_land_where_normalised_motion_puts_them() {
    for (name, ch) in variants() {
        let rig = rig_for(&ch);
        let mut rng = Rng::new(11);
        for round in 0..25 {
            let pose = random_pose(&mut rng, 1.5);
            let mut local = Pose::IDENTITY;
            rig.apply_humanoid(&pose, &mut local);
            let world = world_of(&ch, &local);
            let want = expected_world(&ch, &pose);
            for i in 0..ch.skeleton.len() {
                let got = world.local[i];
                let bone = &ch.skeleton.bones()[i].name;
                assert!(
                    (got.translation - want.position[i]).length() < EPS,
                    "{name}, round {round}, {bone}: at {:?}, expected {:?}",
                    got.translation,
                    want.position[i]
                );
                assert!(
                    same_rotation(got.rotation, want.delta[i] * ch.rest_rot[i], EPS),
                    "{name}, round {round}, {bone}: wrong orientation"
                );
            }
        }
    }
}

#[test]
fn the_normalised_rest_pose_leaves_every_bone_at_rest() {
    for (name, ch) in variants() {
        let mut solved = Pose::IDENTITY;
        rig_for(&ch).apply_humanoid(&HumanoidPose::REST, &mut solved);
        for (i, def) in ch.skeleton.bones().iter().enumerate() {
            assert!((solved.local[i].translation - def.rest.translation).length() < 1e-6, "{name}, {}", def.name);
            assert!(same_rotation(solved.local[i].rotation, def.rest.rotation, 1e-5), "{name}, {}", def.name);
        }
    }
}

#[test]
fn a_hips_offset_moves_the_whole_body_along_world_axes_whatever_the_armature_looks_like() {
    let offset = Vec3::new(0.2, -0.1, 0.3);
    for (name, ch) in variants() {
        let mut local = Pose::IDENTITY;
        rig_for(&ch).apply_humanoid(&HumanoidPose { hips_offset: offset, ..HumanoidPose::REST }, &mut local);
        let world = world_of(&ch, &local);
        for i in 0..ch.skeleton.len() {
            let moved = world.local[i].translation - ch.rest_pos[i];
            let expected = if i == 0 { Vec3::ZERO } else { offset }; // bone 0 is the armature root, above the hips
            assert!((moved - expected).length() < EPS, "{name}, {}: moved {moved:?}", ch.skeleton.bones()[i].name);
            assert!(same_rotation(world.local[i].rotation, ch.rest_rot[i], EPS), "{name}: no rotation was asked for");
        }
    }
}

#[test]
fn a_pose_survives_the_round_trip_through_the_rig() {
    for (name, ch) in variants() {
        let rig = rig_for(&ch);
        let mut rng = Rng::new(5);
        for round in 0..25 {
            let pose = random_pose(&mut rng, 2.0);
            let (mut local, mut back) = (Pose::IDENTITY, HumanoidPose::REST);
            rig.apply_humanoid(&pose, &mut local);
            rig.to_humanoid(&local, &mut back);
            assert!((back.hips_offset - pose.hips_offset).length() < EPS, "{name}, round {round}: hips offset");
            for bone in HumanoidBone::ALL {
                let want = if ch.skeleton.humanoid_bone(bone).is_some() { pose.get(bone) } else { Quat::IDENTITY };
                assert!(same_rotation(back.get(bone), want, EPS), "{name}, round {round}: {bone}");
            }
        }
    }
}

#[test]
fn skinning_matrices_carry_points_attached_to_a_bone() {
    for (name, ch) in variants() {
        let rig = rig_for(&ch);
        let mut skinner = Skinner::new(rig.rig_skeleton().clone());
        let attached = Vec3::new(0.1, 0.2, -0.3);
        let mut rng = Rng::new(23);
        let mut skin = vec![Mat4::ZERO; ch.skeleton.len()];
        for round in 0..15 {
            let pose = random_pose(&mut rng, 1.5);
            let mut local = Pose::IDENTITY;
            rig.apply_humanoid(&pose, &mut local);
            skinner.skin(&local, &mut skin);
            let want = expected_world(&ch, &pose);
            for (i, matrix) in skin.iter().enumerate() {
                // A vertex bound to bone `i` at a rest offset from it ends up at the bone's new position plus the
                // same offset turned by the bone's world rotation.
                let vertex = ch.rest_pos[i] + attached;
                let moved = matrix.transform_point3(vertex);
                let expected = want.position[i] + want.delta[i] * attached;
                assert!(
                    (moved - expected).length() < EPS,
                    "{name}, round {round}, {}: vertex at {moved:?}, expected {expected:?}",
                    ch.skeleton.bones()[i].name
                );
            }
        }
    }
}

#[test]
fn the_rest_pose_skins_to_identity_on_every_rig() {
    for (name, ch) in variants() {
        let rig = rig_for(&ch);
        let mut skinner = Skinner::new(rig.rig_skeleton().clone());
        let (mut local, mut skin) = (Pose::IDENTITY, vec![Mat4::ZERO; ch.skeleton.len()]);
        rig.apply_humanoid(&HumanoidPose::REST, &mut local);
        skinner.skin(&local, &mut skin);
        assert!(skin.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)), "{name}: no pose, no skinning");
    }
}
