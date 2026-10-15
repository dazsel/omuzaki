//! A character with many hair chains, shared by the spring benchmark and the allocation test (each includes this file
//! with `#[path]`, so the two always measure the same rig).

#![allow(dead_code)] // each includer uses a different part
#![allow(clippy::expect_used)] // a rig that fails to build should stop the run with its reason

use omuzaki_anim_api::{
    BoneDef, BoneId, Collider, ColliderGroup, ColliderShape, HumanoidBone, Pose, Skeleton, SpringChain, SpringParams,
    SpringSet, Transform,
};
use omuzaki_core::glam::{EulerRot, Quat, Vec3};

/// The humanoid, its hair and the colliders the hair collides with.
pub struct HairRig {
    pub skeleton: Skeleton,
    pub set: SpringSet,
    pub head: BoneId,
    pub hips: BoneId,
}

/// `chains` chains of `joints` joints each (the last joint of each is only a tail), hanging from the head, all
/// colliding with a head sphere and a spine capsule. Every third chain is simulated relative to the head.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // a few hundred bones
pub fn hair_rig(chains: usize, joints: usize) -> HairRig {
    let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
    let base = Skeleton::humanoid(&required, |b| {
        Transform::from_translation(if b == HumanoidBone::Hips { Vec3::Y } else { Vec3::Y * 0.1 })
    })
    .expect("the required humanoid bones form a skeleton");
    let humanoid = |bone| base.humanoid_bone(bone).expect("the base skeleton has the required bones");
    let (hips, head, spine) =
        (humanoid(HumanoidBone::Hips), humanoid(HumanoidBone::Head), humanoid(HumanoidBone::Spine));

    let mut bones = base.bones().to_vec();
    let mut ids = vec![Vec::new(); chains];
    for (c, chain) in ids.iter_mut().enumerate() {
        let mut parent = head;
        for j in 0..joints {
            let id = BoneId(bones.len() as u16);
            let (c_f, j_f) = (c as f32, j as f32);
            bones.push(BoneDef {
                name: format!("hair_{c}_{j}"),
                parent: Some(parent),
                rest: Transform {
                    translation: Vec3::new(0.004 * (c_f - 10.0), -0.04, 0.01 * (j_f % 3.0 - 1.0)),
                    rotation: Quat::from_euler(EulerRot::XYZ, 0.05 * (j_f % 4.0), 0.02 * c_f, 0.08 * (j_f % 5.0)),
                },
                humanoid: None,
            });
            chain.push(id);
            parent = id;
        }
    }
    let skeleton = Skeleton::new(bones).expect("the hair bones follow their parents");

    let params = SpringParams {
        hit_radius: 0.01,
        stiffness: 2.0,
        gravity_power: 1.0,
        drag_force: 0.3,
        ..SpringParams::default()
    };
    let tip = SpringParams { stiffness: 0.5, ..params };
    let chains = ids
        .iter()
        .enumerate()
        .map(|(i, bones)| {
            let mut chain = SpringChain::tapered(format!("hair_{i}"), bones, params, tip);
            chain.collider_groups = vec!["body".into()];
            chain.center = (i % 3 == 0).then_some(head);
            chain
        })
        .collect();
    let body = ColliderGroup {
        name: "body".into(),
        colliders: vec![
            Collider { bone: head, shape: ColliderShape::Sphere { offset: Vec3::new(0.0, -0.05, 0.0), radius: 0.1 } },
            Collider {
                bone: spine,
                shape: ColliderShape::Capsule { offset: Vec3::ZERO, radius: 0.08, tail: Vec3::new(0.0, 0.2, 0.0) },
            },
        ],
    };
    HairRig { skeleton, set: SpringSet { chains, groups: vec![body] }, head, hips }
}

/// [`hair_rig`] whose chains also collide with an inside sphere, an inside capsule and a plane on the head, so the
/// new shapes are measured and checked for allocation on the same rig as the old ones.
pub fn extended_hair_rig(chains: usize, joints: usize) -> HairRig {
    let mut rig = hair_rig(chains, joints);
    let shape = |shape| Collider { bone: rig.head, shape };
    let colliders = vec![
        shape(ColliderShape::SphereInside { offset: Vec3::new(0.0, -0.1, 0.0), radius: 0.3 }),
        shape(ColliderShape::CapsuleInside {
            offset: Vec3::new(0.0, -0.05, 0.0),
            radius: 0.25,
            tail: Vec3::new(0.0, -0.3, 0.05),
        }),
        shape(ColliderShape::Plane {
            offset: Vec3::new(0.0, -0.35, 0.0),
            normal: Vec3::new(0.0, 1.0, 0.1).normalize(),
        }),
    ];
    rig.set.groups.push(ColliderGroup { name: "extended".into(), colliders });
    for chain in &mut rig.set.chains {
        chain.collider_groups.push("extended".into());
    }
    rig
}

impl HairRig {
    /// The animated pose at `frame`: the hips travel and turn and the head nods, so the hair never rests.
    #[allow(clippy::cast_precision_loss)] // frame counts stay small
    pub fn animate(&self, pose: &mut Pose, frame: u64) {
        let t = frame as f32;
        pose.local[self.hips.index()].translation = Vec3::new(0.3 * (0.07 * t).sin(), 1.0, 0.1 * (0.05 * t).cos());
        pose.local[self.head.index()].rotation =
            Quat::from_euler(EulerRot::XYZ, 0.3 * (0.13 * t).sin(), 0.5 * (0.09 * t).sin(), 0.0);
    }
}
