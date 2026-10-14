//! Adversarial rest poses for the humanoid solve: every bone carries a 90-degree twist, the armature
//! root is turned, a non-humanoid root sits above the hips, and non-humanoid helper bones (twist / chest helpers, as
//! `VRoid` and Blender rigs have) sit *between* humanoid bones, with optional bones absent. The expectation is the
//! independent normalised-motion walk in `common::expected_world`.

#![allow(clippy::unwrap_used, clippy::needless_range_loop)] // test helpers may panic; indexing several parallel tables by bone

mod common;

use std::f32::consts::FRAC_PI_2;

use common::{Character, expected_world, random_pose, same_rotation, tpose_offset};
use omuzaki_anim_api::{BoneDef, BoneId, HumanoidBone, HumanoidPose, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;

const EPS: f32 = 1e-4;

#[derive(Default)]
struct B {
    defs: Vec<BoneDef>,
    pos: Vec<Vec3>,
    rot: Vec<Quat>,
}

impl B {
    fn add(
        &mut self,
        name: &str,
        parent: Option<usize>,
        humanoid: Option<HumanoidBone>,
        pos: Vec3,
        rot: Quat,
    ) -> usize {
        let (pp, pr) = parent.map_or((Vec3::ZERO, Quat::IDENTITY), |p| (self.pos[p], self.rot[p]));
        let inv = pr.inverse();
        self.defs.push(BoneDef {
            name: name.to_owned(),
            parent: parent.map(|p| BoneId(u16::try_from(p).unwrap())),
            rest: Transform { translation: inv * (pos - pp), rotation: (inv * rot).normalize() },
            humanoid,
        });
        self.pos.push(pos);
        self.rot.push(rot);
        self.defs.len() - 1
    }
}

fn twist(i: usize) -> Quat {
    let axes = [Vec3::X, Vec3::Y, Vec3::Z, Vec3::new(1.0, 1.0, 0.0).normalize(), Vec3::new(0.0, 1.0, -1.0).normalize()];
    Quat::from_axis_angle(axes[i % axes.len()], FRAC_PI_2)
}

fn position(bone: HumanoidBone) -> Vec3 {
    std::iter::successors(Some(bone), |b| b.parent()).map(tpose_offset).sum()
}

fn character() -> Character {
    let mut b = B::default();
    let armature = b.add("armature", None, None, Vec3::ZERO, Quat::from_rotation_x(FRAC_PI_2));
    let root = b.add("root_twist", Some(armature), None, Vec3::ZERO, Quat::from_rotation_y(FRAC_PI_2));
    let mut index = [None; omuzaki_anim_api::HUMANOID_COUNT];
    let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|x| x.is_required()).collect();
    for (n, bone) in HumanoidBone::ALL.into_iter().filter(|x| required.contains(x)).enumerate() {
        let mut parent =
            std::iter::successors(bone.parent(), |p| p.parent()).find_map(|p| index[p.index()]).unwrap_or(root);
        // A non-humanoid helper between the humanoid parent and these bones.
        let helper_name = match bone {
            HumanoidBone::Head => Some("chest_helper"),
            HumanoidBone::LeftLowerArm => Some("l_twist_helper"),
            HumanoidBone::RightLowerArm => Some("r_twist_helper"),
            _ => None,
        };
        if let Some(name) = helper_name {
            let p = b.pos[parent];
            parent = b.add(name, Some(parent), None, (p + position(bone)) * 0.5 + p * 0.5, twist(n + 2));
        }
        index[bone.index()] = Some(b.add(bone.name(), Some(parent), Some(bone), position(bone), twist(n)));
    }
    Character { skeleton: Skeleton::new(b.defs).unwrap(), rest_pos: b.pos, rest_rot: b.rot }
}

fn rig_for(ch: &Character) -> HumanoidRig {
    HumanoidRig::new(RigSkeleton::new(ch.skeleton.clone()))
}

#[test]
fn twisted_rest_poses_with_helper_bones_between_humanoid_bones_land_where_normalised_motion_puts_them() {
    let ch = character();
    let rig = rig_for(&ch);
    let mut skinner = Skinner::new(rig.rig_skeleton().clone());
    let mut rng = Rng::new(99);
    let mut skin = vec![Mat4::ZERO; ch.skeleton.len()];
    for round in 0..50 {
        let pose = random_pose(&mut rng, 2.5);
        let mut local = Pose::IDENTITY;
        rig.apply_humanoid(&pose, &mut local);
        let mut world = Pose::IDENTITY;
        ch.skeleton.world_pose(&local, &mut world);
        let want = expected_world(&ch, &pose);
        skinner.skin(&local, &mut skin);
        for i in 0..ch.skeleton.len() {
            let name = &ch.skeleton.bones()[i].name;
            assert!((world.local[i].translation - want.position[i]).length() < EPS, "round {round}, {name}: position");
            assert!(
                same_rotation(world.local[i].rotation, want.delta[i] * ch.rest_rot[i], EPS),
                "round {round}, {name}: rotation"
            );
            // Skinning keeps handedness: a rigid motion, determinant +1.
            assert!((skin[i].determinant() - 1.0).abs() < 1e-4, "round {round}, {name}: determinant");
            let vertex = ch.rest_pos[i] + Vec3::new(0.1, 0.2, -0.3);
            let moved = skin[i].transform_point3(vertex);
            let expected = want.position[i] + want.delta[i] * Vec3::new(0.1, 0.2, -0.3);
            assert!((moved - expected).length() < EPS, "round {round}, {name}: skinned vertex");
        }
        // Optional bones are absent: their rotations must be ignored, and read back as identity.
        let mut back = HumanoidPose::REST;
        rig.to_humanoid(&local, &mut back);
        assert!((back.hips_offset - pose.hips_offset).length() < EPS);
        for bone in HumanoidBone::ALL {
            let want = if ch.skeleton.humanoid_bone(bone).is_some() { pose.get(bone) } else { Quat::IDENTITY };
            assert!(same_rotation(back.get(bone), want, EPS), "round {round}: round trip {bone}");
        }
    }
}

#[test]
fn a_negated_or_scaled_quaternion_is_the_same_pose() {
    let ch = character();
    let rig = rig_for(&ch);
    let mut rng = Rng::new(5);
    let pose = random_pose(&mut rng, 2.0);
    let mut weird = pose;
    for (i, q) in weird.rotations.iter_mut().enumerate() {
        let k = if i % 2 == 0 { -3.0 } else { 0.25 };
        *q = Quat::from_xyzw(q.x * k, q.y * k, q.z * k, q.w * k);
    }
    let (mut a, mut b) = (Pose::IDENTITY, Pose::IDENTITY);
    rig.apply_humanoid(&pose, &mut a);
    rig.apply_humanoid(&weird, &mut b);
    for i in 0..ch.skeleton.len() {
        assert!(same_rotation(a.local[i].rotation, b.local[i].rotation, 1e-5), "bone {i}");
        assert!(b.local[i].rotation.is_normalized(), "bone {i} not unit");
    }
}

#[test]
fn a_scaled_or_mirrored_rest_cannot_enter_the_rig() {
    // `Transform` has no scale and `Skeleton::new` insists on a unit rotation, so a non-uniformly scaled or mirrored
    // (negative-determinant) bind has to be baked out by the importer; the solve never sees one.
    let ch = character();
    let mut defs: Vec<BoneDef> = ch.skeleton.bones().to_vec();
    defs[0].rest.rotation = Quat::from_xyzw(2.0, 0.0, 0.0, 0.0); // what a scale of 2 would look like
    assert!(Skeleton::new(defs).is_err());
}
