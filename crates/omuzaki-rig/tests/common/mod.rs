//! Shared fixtures for the rig's tests and benchmark: a VRM-proportioned T-pose character on a skeleton whose bone
//! axes are either aligned with the world (the VRM 1.0 normalised look) or tilted at random (what Blender and VRM 0.x
//! exports look like), with an armature root above the hips and spring-bone style hair chains below the head.
//!
//! The fixture also records the *facts* it was built from (world rest positions and orientations) so tests can compute
//! their expectations independently of the code under test.

// Each test binary uses a different subset; fixture helpers may panic (a panic is a clear test failure).
#![allow(dead_code, clippy::unwrap_used, clippy::cast_precision_loss)]

use omuzaki_anim_api::{BoneDef, BoneId, HumanoidBone, HumanoidPose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;

/// A skeleton and the world-space rest facts behind it.
pub struct Character {
    /// The validated skeleton (armature root, then humanoid bones, then hair).
    pub skeleton: Skeleton,
    /// World-space rest position of each bone, metres.
    pub rest_pos: Vec<Vec3>,
    /// World-space rest orientation of each bone (identity for every bone when the axes are aligned).
    pub rest_rot: Vec<Quat>,
}

impl Character {
    /// The skeleton index of a humanoid bone.
    pub fn id(&self, bone: HumanoidBone) -> usize {
        self.skeleton.humanoid_bone(bone).unwrap().index()
    }
}

fn side(bone: HumanoidBone) -> f32 {
    match bone.name() {
        n if n.starts_with("left") => 1.0,
        n if n.starts_with("right") => -1.0,
        _ => 0.0,
    }
}

/// Where `bone` sits relative to its VRM parent in the T-pose, in world axes (+X is the character's left).
pub fn tpose_offset(bone: HumanoidBone) -> Vec3 {
    use HumanoidBone as B;
    let s = side(bone);
    match bone {
        B::Hips => Vec3::new(0.0, 0.95, 0.0),
        B::Chest | B::UpperChest => Vec3::new(0.0, 0.12, 0.0),
        B::Spine | B::Neck => Vec3::new(0.0, 0.10, 0.0),
        B::Head => Vec3::new(0.0, 0.06, 0.0),
        B::LeftEye | B::RightEye => Vec3::new(s * 0.03, 0.07, 0.08),
        B::Jaw => Vec3::new(0.0, 0.03, 0.04),
        B::LeftUpperLeg | B::RightUpperLeg => Vec3::new(s * 0.08, -0.05, 0.0),
        B::LeftLowerLeg | B::RightLowerLeg | B::LeftFoot | B::RightFoot => Vec3::new(0.0, -0.42, 0.0),
        B::LeftToes | B::RightToes => Vec3::new(0.0, -0.07, 0.12),
        B::LeftShoulder | B::RightShoulder => Vec3::new(s * 0.03, 0.08, 0.0),
        B::LeftUpperArm | B::RightUpperArm => Vec3::new(s * 0.12, 0.0, 0.0),
        B::LeftLowerArm | B::RightLowerArm => Vec3::new(s * 0.27, 0.0, 0.0),
        B::LeftHand | B::RightHand => Vec3::new(s * 0.25, 0.0, 0.0),
        _ => Vec3::new(s * 0.03, 0.0, 0.0), // finger joints
    }
}

fn tpose_position(bone: HumanoidBone) -> Vec3 {
    std::iter::successors(Some(bone), |b| b.parent()).map(tpose_offset).sum()
}

fn random_rotation(rng: &mut Rng, max_angle: f32) -> Quat {
    let axis = loop {
        let v = Vec3::new(rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0));
        if let Some(unit) = v.try_normalize() {
            break unit;
        }
    };
    Quat::from_axis_angle(axis, rng.range_f32(-max_angle, max_angle))
}

/// Collects bones given by their world-space facts and expresses each in its parent's frame, the way an exporter would.
#[derive(Default)]
struct Builder {
    defs: Vec<BoneDef>,
    rest_pos: Vec<Vec3>,
    rest_rot: Vec<Quat>,
}

impl Builder {
    /// Adds a bone at world position `pos` with world orientation `rot` under `parent`; returns its index.
    fn add(
        &mut self,
        name: String,
        parent: Option<usize>,
        humanoid: Option<HumanoidBone>,
        pos: Vec3,
        rot: Quat,
    ) -> usize {
        let (parent_pos, parent_rot) =
            parent.map_or((Vec3::ZERO, Quat::IDENTITY), |p| (self.rest_pos[p], self.rest_rot[p]));
        let into_parent = parent_rot.inverse();
        self.defs.push(BoneDef {
            name,
            parent: parent.map(|p| BoneId(u16::try_from(p).unwrap())),
            rest: Transform {
                translation: into_parent * (pos - parent_pos),
                rotation: (into_parent * rot).normalize(),
            },
            humanoid,
        });
        self.rest_pos.push(pos);
        self.rest_rot.push(rot);
        self.defs.len() - 1
    }
}

/// Builds a character with the humanoid `bones`, `hair_chains` chains of `hair_len` bones under the head, and (when
/// `tilted`) random rest orientations on every bone, the armature root included.
///
/// `bones` must include the 15 required ones. With 55 humanoid bones, one root and 9 chains of 8 this is the
/// 128-joint skeleton the benchmark uses.
pub fn character(bones: &[HumanoidBone], tilted: bool, hair_chains: usize, hair_len: usize, seed: u64) -> Character {
    let mut rng = Rng::new(seed);
    let orientation = |rng: &mut Rng| if tilted { random_rotation(rng, 3.0) } else { Quat::IDENTITY };
    let mut b = Builder::default();

    let root = b.add("armature".into(), None, None, Vec3::ZERO, orientation(&mut rng));
    let mut index = [None; omuzaki_anim_api::HUMANOID_COUNT];
    for bone in HumanoidBone::ALL.into_iter().filter(|b| bones.contains(b)) {
        let parent = std::iter::successors(bone.parent(), |p| p.parent()).find_map(|p| index[p.index()]).or(Some(root));
        let tilt = orientation(&mut rng);
        index[bone.index()] = Some(b.add(bone.name().to_owned(), parent, Some(bone), tpose_position(bone), tilt));
    }
    let head = index[HumanoidBone::Head.index()].unwrap();
    for chain in 0..hair_chains {
        let mut parent = head;
        for i in 0..hair_len {
            let pos = if i == 0 {
                b.rest_pos[head] + Vec3::new(0.01 * chain as f32, 0.04, -0.07)
            } else {
                b.rest_pos[parent] + Vec3::new(0.0, -0.06, 0.005)
            };
            let tilt = orientation(&mut rng);
            parent = b.add(format!("hair_{chain}_{i}"), Some(parent), None, pos, tilt);
        }
    }
    Character { skeleton: Skeleton::new(b.defs).unwrap(), rest_pos: b.rest_pos, rest_rot: b.rest_rot }
}

/// All 55 humanoid bones, an armature root and 9 hair chains of 8: 128 joints.
pub fn full_character(tilted: bool) -> Character {
    character(&HumanoidBone::ALL, tilted, 9, 8, 7)
}

/// A pose with every bone turned by up to `spread` radians about a random axis and the hips moved by up to 0.3 m.
pub fn random_pose(rng: &mut Rng, spread: f32) -> HumanoidPose {
    let mut pose = HumanoidPose::REST;
    for bone in HumanoidBone::ALL {
        pose.set(bone, random_rotation(rng, spread));
    }
    pose.hips_offset = Vec3::new(rng.range_f32(-0.3, 0.3), rng.range_f32(-0.3, 0.3), rng.range_f32(-0.3, 0.3));
    pose
}

/// Where every bone must end up for `pose`, computed the way VRM defines normalised motion and without any of the
/// rig's code: walk the hierarchy keeping a world-axes rotation `D` per bone, where a humanoid bone adds its
/// normalised rotation on top of its parent's, a bone moves its children's rest offset by `D`, and the hips add the
/// hips offset. A rig is correct when its forward kinematics gives `position` and `D · rest orientation`.
pub struct Expected {
    /// World position of each bone.
    pub position: Vec<Vec3>,
    /// World rotation applied on top of the bone's rest orientation (`D`).
    pub delta: Vec<Quat>,
}

pub fn expected_world(ch: &Character, pose: &HumanoidPose) -> Expected {
    let n = ch.skeleton.len();
    let (mut position, mut delta) = (vec![Vec3::ZERO; n], vec![Quat::IDENTITY; n]);
    for (i, def) in ch.skeleton.bones().iter().enumerate() {
        let turn = def.humanoid.map_or(Quat::IDENTITY, |h| pose.get(h));
        let shift = if def.humanoid == Some(HumanoidBone::Hips) { pose.hips_offset } else { Vec3::ZERO };
        match def.parent {
            None => {
                delta[i] = turn;
                position[i] = ch.rest_pos[i] + shift;
            }
            Some(p) => {
                let p = p.index();
                delta[i] = delta[p] * turn;
                position[i] = position[p] + delta[p] * (ch.rest_pos[i] - ch.rest_pos[p]) + shift;
            }
        }
    }
    Expected { position, delta }
}

/// True if `a` and `b` are the same rotation (a quaternion and its negation are).
pub fn same_rotation(a: Quat, b: Quat, eps: f32) -> bool {
    a.abs_diff_eq(if a.dot(b) < 0.0 { -b } else { b }, eps)
}
