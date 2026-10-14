//! Non-humanoid characters through the rig (CB-05): a quadruped and a blob are built with `Skeleton::generic`,
//! prepared, skinned and driven by the spring solver, and the humanoid stage declines them without a panic.
//!
//! The expectations are worked out from the rest pose alone (rigid bones turn about the root), so they do not use the
//! code under test.

// Test helpers may panic (a panic is a clear test failure); frame counters are tiny, so the float casts are exact.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::sync::Arc;

use omuzaki_anim_api::{BoneId, HumanoidBone, HumanoidPose, NotHumanoid, Pose, Rig, SpringSolver, Tick};
use omuzaki_anim_mock::fixtures::{
    BLOB_BONES, CreatureRig, QUADRUPED_BONES, blob_rig, humanoid_skeleton, quadruped_rig,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3, Vec4};
use omuzaki_render_api::JointRange;
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;
use omuzaki_rig::spring::VrmSpringSolver;

const ROOT: BoneId = BoneId(0);

fn creatures() -> Vec<(&'static str, CreatureRig, usize)> {
    vec![
        ("quadruped", quadruped_rig().expect("fixture"), QUADRUPED_BONES),
        ("blob", blob_rig().expect("fixture"), BLOB_BONES),
    ]
}

/// The creature rocks side to side and turns about its vertical axis; every other animated bone stays at rest.
fn sway(pose: &mut Pose, rest: &Pose, frame: u64) {
    let t = frame as f32 / 60.0;
    pose.local[ROOT.index()].translation =
        rest.local[ROOT.index()].translation + Vec3::new(0.3 * (t * 6.0).sin(), 0.0, 0.0);
    pose.local[ROOT.index()].rotation = Quat::from_rotation_y(0.5 * (t * 5.0).sin());
}

fn spring_bones(creature: &CreatureRig) -> Vec<BoneId> {
    creature.springs.chains.iter().flat_map(|c| c.joints.iter().map(|j| j.bone)).collect()
}

#[test]
fn a_creature_is_prepared_for_the_rig() {
    for (name, creature, bones) in creatures() {
        let rig = RigSkeleton::new(creature.skeleton);
        assert_eq!((rig.len(), rig.rest_local().len(), rig.inverse_bind().len()), (bones, bones, bones), "{name}");
        for i in 0..bones {
            let id = BoneId(u16::try_from(i).unwrap());
            let bind = rig.rest_world(id).unwrap().to_mat4();
            assert!((bind * rig.inverse_bind()[i]).abs_diff_eq(Mat4::IDENTITY, 1e-5), "{name}, bone {i}");
        }
    }
}

#[test]
fn a_creature_skins_to_identity_at_rest_and_every_bone_follows_a_turned_root() {
    for (name, creature, bones) in creatures() {
        let rig = RigSkeleton::new(creature.skeleton);
        let mut skinner = Skinner::new(Arc::clone(&rig));
        let mut palette = vec![Mat4::IDENTITY; 4];
        let rest = rig.skeleton().rest_pose();
        let range = skinner.push(&rest, &mut palette);
        assert_eq!(range, JointRange { start: 4, len: u32::try_from(bones).unwrap() }, "{name}");
        assert!(palette[4..].iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)), "{name}: no pose, no skinning");

        // Turn the root 90 degrees about Z. Each bone has moved rigidly about the root's rest position, so a point bound
        // at the bone's own rest position lands at root + R * (rest - root).
        let turn = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let mut posed = rest.clone();
        posed.local[ROOT.index()].rotation = turn;
        let mut skin = vec![Mat4::ZERO; bones];
        skinner.skin(&posed, &mut skin);
        let origin = rig.rest_world(ROOT).unwrap().translation;
        for (i, matrix) in skin.iter().enumerate() {
            let bound = rig.rest_world(BoneId(u16::try_from(i).unwrap())).unwrap().translation;
            let want = origin + turn * (bound - origin);
            let got = matrix.transform_point3(bound);
            assert!((got - want).length() < 1e-5, "{name}, bone {i}: at {got:?}, expected {want:?}");
        }
    }
}

#[test]
fn the_humanoid_stage_has_nothing_to_drive_on_a_creature_and_try_new_says_so() {
    let mut wild = HumanoidPose::REST;
    wild.hips_offset = Vec3::new(1.0, 2.0, 3.0);
    for (bone, angle) in HumanoidBone::ALL.into_iter().zip((1..).map(|i| 0.1 * i as f32)) {
        wild.set(bone, Quat::from_rotation_x(angle));
    }
    for (name, creature, bones) in creatures() {
        let skeleton = RigSkeleton::new(creature.skeleton);
        assert_eq!(HumanoidRig::try_new(Arc::clone(&skeleton)).err(), Some(NotHumanoid), "{name}");

        // The lenient constructor is the explicit fallback: whatever the pose says, every bone stays at rest.
        let mut rig = HumanoidRig::new(Arc::clone(&skeleton));
        let mut out = Pose::IDENTITY;
        rig.solve(Tick::at_rate(3, 60.0), &wild, &mut out);
        assert_eq!(&out.local[..bones], skeleton.rest_local(), "{name}");
        let mut back = wild;
        rig.to_humanoid(&out, &mut back);
        assert_eq!(back, HumanoidPose::REST, "{name}: nothing to read back");
        rig.reset();
        assert!(!Rig::skeleton(&rig).is_humanoid());
    }
}

#[test]
fn a_humanoid_is_still_accepted_by_try_new() {
    let skeleton = RigSkeleton::new(humanoid_skeleton().expect("fixture"));
    assert!(HumanoidRig::try_new(skeleton).is_ok());
}

/// The numbers quoted in the PR: how far the soft parts swing (largest angle from rest, over the run).
#[test]
fn springs_swing_the_soft_parts_of_a_creature_and_leave_the_rest_alone() {
    for (name, creature, bones) in creatures() {
        let CreatureRig { skeleton, springs } = &creature;
        let soft = spring_bones(&creature);
        let mut solver = VrmSpringSolver::new(springs, skeleton).expect("the creature's springs are valid");
        let rest = skeleton.rest_pose();
        let mut pose = rest.clone();
        let mut swing = vec![0.0_f32; bones];
        for frame in 0..180 {
            sway(&mut pose, &rest, frame);
            solver.step(Tick::at_rate(frame, 60.0), skeleton, &mut pose);
            for (i, angle) in swing.iter_mut().enumerate() {
                assert!(
                    pose.local[i].rotation.is_finite() && pose.local[i].translation.is_finite(),
                    "{name}, bone {i}"
                );
                *angle = angle.max(pose.local[i].rotation.angle_between(rest.local[i].rotation));
            }
        }
        for chain in &springs.chains {
            let most = chain.joints.iter().map(|j| swing[j.bone.index()]).fold(0.0, f32::max);
            println!("{name}: chain {:?} swings up to {most:.3} rad", chain.name);
            assert!(most > 0.02, "{name}: chain {:?} barely moved ({most})", chain.name);
        }
        for (i, angle) in swing.iter().enumerate() {
            let id = BoneId(u16::try_from(i).unwrap());
            // The root is animated by the test; its rotation is not the solver's doing.
            if id != ROOT && !soft.contains(&id) {
                assert_eq!(
                    *angle,
                    0.0,
                    "{name}: {} is not a spring bone but the solver turned it",
                    skeleton.bones()[i].name
                );
            }
        }
    }
}

#[test]
fn creature_springs_are_deterministic_and_settle() {
    for (name, creature, _) in creatures() {
        let CreatureRig { skeleton, springs } = &creature;
        let rest = skeleton.rest_pose();
        let run = |solver: &mut VrmSpringSolver| {
            let mut pose = rest.clone();
            for frame in 0..90 {
                sway(&mut pose, &rest, frame);
                solver.step(Tick::at_rate(frame, 60.0), skeleton, &mut pose);
            }
            pose
        };
        let mut a = VrmSpringSolver::new(springs, skeleton).expect("valid");
        let mut b = VrmSpringSolver::new(springs, skeleton).expect("valid");
        let first = run(&mut a);
        assert_eq!(first, run(&mut b), "{name}: two solvers, same inputs, same bits");
        a.reset();
        assert_eq!(first, run(&mut a), "{name}: a reset solver replays the same run");

        // Once the creature stops, the springs come to rest (gravity leaves them slightly sagged, not swinging).
        let mut pose = rest.clone();
        for frame in 0..600 {
            a.step(Tick::at_rate(frame, 60.0), skeleton, &mut pose);
        }
        let settled = pose.clone();
        a.step(Tick::at_rate(600, 60.0), skeleton, &mut pose);
        // Compared by components: `angle_between` of near-equal unit quaternions is only good to about 1e-3 rad.
        let drift = (0..skeleton.len())
            .map(|i| Vec4::from(settled.local[i].rotation).distance(Vec4::from(pose.local[i].rotation)))
            .fold(0.0, f32::max);
        assert!(drift < 1e-5, "{name}: still moving after ten seconds ({drift} per frame)");
    }
}

#[test]
fn a_spring_driven_creature_skins_to_finite_matrices_and_only_the_soft_parts_leave_the_root() {
    for (name, creature, bones) in creatures() {
        let CreatureRig { skeleton, springs } = &creature;
        let soft = spring_bones(&creature);
        let mut skinner = Skinner::new(RigSkeleton::new(skeleton.clone()));
        let mut solver = VrmSpringSolver::new(springs, skeleton).expect("valid");
        let rest = skeleton.rest_pose();
        let mut pose = rest.clone();
        let mut skin = vec![Mat4::ZERO; bones];
        for frame in 0..120 {
            sway(&mut pose, &rest, frame);
            solver.step(Tick::at_rate(frame, 60.0), skeleton, &mut pose);
            skinner.skin(&pose, &mut skin);
        }
        assert!(skin.iter().all(Mat4::is_finite), "{name}");
        // Bones nothing animates ride the root rigidly (their skinning matrix is the root's); the swinging ones do not.
        for (i, matrix) in skin.iter().enumerate() {
            if !soft.contains(&BoneId(u16::try_from(i).unwrap())) {
                assert!(matrix.abs_diff_eq(skin[ROOT.index()], 1e-5), "{name}: bone {i} should ride the root");
            }
        }
        assert!(
            soft.iter().any(|b| !skin[b.index()].abs_diff_eq(skin[ROOT.index()], 1e-3)),
            "{name}: after two seconds of swaying some soft bone has left the root's frame"
        );
    }
}
