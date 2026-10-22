//! Landmarks measured on the built subject in its bind pose (top, bottom, crown, chin, eyes, chest, hands, height).
//!
//! Framing (design D5, ADR 0024) reads these numbers and nothing else of the character. Everything is metres in the
//! character's own frame (feet near `y = 0`, facing +Z). The rig is not posed: the build is the bind pose
//! (ADR 0014), so positions are read as built.

use omuzaki_anim_api::{HumanoidBone, Pose};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_char_api::{BuiltCharacter, JointRef, PartCategory, PartMesh, find_kind};
use omuzaki_core::glam::Vec3;

use crate::error::StageError;

/// An axis-aligned box in the character's bind space, metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    /// The corner with the smallest coordinates.
    pub min: Vec3,
    /// The corner with the largest coordinates.
    pub max: Vec3,
}

impl Bounds {
    /// The box's extent along Y.
    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }

    /// The middle of the box.
    pub fn centre(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// The box's width seen by a camera at `yaw_deg` around +Y (0 = from +Z):
    /// `(max.x - min.x) * |cos yaw| + (max.z - min.z) * |sin yaw|`.
    pub fn width_at(&self, yaw_deg: f32) -> f32 {
        let (sin, cos) = yaw_deg.to_radians().sin_cos();
        (self.max.x - self.min.x) * cos.abs() + (self.max.z - self.min.z) * sin.abs()
    }

    /// Grows the box in `slot` to hold `point`, starting it there if `slot` is empty.
    fn include(slot: &mut Option<Self>, point: Vec3) {
        match slot {
            Some(bounds) => {
                bounds.min = bounds.min.min(point);
                bounds.max = bounds.max.max(point);
            }
            None => *slot = Some(Self { min: point, max: point }),
        }
    }
}

/// What framing measures on a built character in its bind pose (design D5): metres in the character's own frame
/// (feet near y = 0, facing +Z).
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Landmarks {
    /// Every vertex of every mesh, hair and hats included. `top` is `body.max.y`, `bottom` is `body.min.y`.
    pub body: Bounds,
    /// The head part's head mesh. `crown` is `max.y`, `chin` is `min.y`. `None` without a head part.
    pub head: Option<Bounds>,
    /// The eye line: the mean bind height of the `leftEye` and `rightEye` joints (one of them if only one exists).
    pub eyes: Option<f32>,
    /// The bind height of the `upperChest` joint, else of `chest`.
    pub chest: Option<f32>,
    /// Every vertex at or above `chest`: the bust shot's width. `None` when `chest` is, or when no vertex reaches it.
    pub bust: Option<Bounds>,
    /// Both hands: every vertex whose largest skin weight is on a hand or finger bone.
    pub hands: Option<Bounds>,
    /// `BuiltCharacter::stature`'s `height_m`, else `body.height()`.
    pub height_m: f32,
}

impl Landmarks {
    /// Measures `built` in its bind pose.
    ///
    /// The head part is the first (by part id) enabled part whose kind is registered with
    /// [`PartCategory::Head`]; its head mesh is the one of that part named `head` or ending in `-head`, which leaves
    /// out the neck and the ears. Joint heights are read from the bind pose of the rig, as the face system reads the
    /// head.
    ///
    /// # Errors
    /// [`StageError::Framing`] for a character with no vertex, a non-finite position or joint height, a height (the stature's, else the
    /// body's extent in Y) that isn't finite and above zero, or a skinned mesh whose joint data doesn't fit its vertices.
    pub fn measure(built: &BuiltCharacter) -> Result<Self, StageError> {
        let inverse_bind = built.rig.inverse_bind();
        let joint_height = |bone: HumanoidBone| {
            let id = built.rig.bone(&JointRef::Humanoid(bone))?;
            Some(inverse_bind.get(id.index()).map_or(f32::NAN, |m| m.inverse().w_axis.y))
        };
        let head_part = head_part(built);
        let meshes = built.meshes.iter().map(|built_mesh| Measured {
            mesh: &built_mesh.mesh,
            owner: format!("part '{}'", built_mesh.part),
            head: if head_part == Some(&built_mesh.part) && is_head_mesh(&built_mesh.mesh.name) {
                HeadRule::Whole
            } else {
                HeadRule::No
            },
        });
        measure_meshes(meshes, joint_height, built.stature.map(|s| s.height_m))
    }

    /// Measures an imported avatar in its bind pose, the pose its meshes are stored in (`docs/design/byom.md`), so an
    /// imported character frames like a built one.
    ///
    /// An avatar has no head part: its head is every vertex whose strongest joint is the `head` bone itself, which
    /// leaves out the neck and hair on spring bones. Joint heights are read from the inverse bind of a mesh skinned
    /// to the bone, else from the skeleton's rest. There is no stature, so the height is the body's extent.
    ///
    /// # Errors
    /// As [`measure`](Self::measure).
    pub fn measure_avatar(avatar: &ImportedAvatar) -> Result<Self, StageError> {
        let skeleton = &avatar.skeleton;
        let resolve = |joint: &JointRef| match joint {
            JointRef::Humanoid(bone) => skeleton.humanoid_bone(*bone),
            JointRef::Named(name) => skeleton.find(name.as_str()),
        };
        let mut rest_world = Box::new(Pose::IDENTITY);
        skeleton.world_pose(&skeleton.rest_pose(), &mut rest_world);
        let joint_height = |bone: HumanoidBone| {
            let id = skeleton.humanoid_bone(bone)?;
            let from_mesh = avatar.meshes.iter().find_map(|m| {
                let k = m.mesh.joint_bones.iter().position(|j| resolve(j) == Some(id))?;
                m.inverse_bind.get(k).map(|ibm| ibm.inverse().w_axis.y)
            });
            Some(from_mesh.unwrap_or_else(|| rest_world.local.get(id.index()).map_or(f32::NAN, |t| t.translation.y)))
        };
        let head = skeleton.humanoid_bone(HumanoidBone::Head);
        let meshes = avatar.meshes.iter().map(|m| Measured {
            mesh: &m.mesh,
            owner: "the avatar".to_owned(),
            head: m
                .mesh
                .joint_bones
                .iter()
                .position(|j| head.is_some() && resolve(j) == head)
                .and_then(|k| u16::try_from(k).ok())
                .map_or(HeadRule::No, HeadRule::Joint),
        });
        measure_meshes(meshes, joint_height, None)
    }

    /// The highest point of the character, hair and hats included.
    pub fn top(&self) -> f32 {
        self.body.max.y
    }

    /// The lowest point of the character.
    pub fn bottom(&self) -> f32 {
        self.body.min.y
    }

    /// The top of the head mesh; `None` without a head part.
    pub fn crown(&self) -> Option<f32> {
        self.head.map(|h| h.max.y)
    }

    /// The bottom of the head mesh; `None` without a head part.
    pub fn chin(&self) -> Option<f32> {
        self.head.map(|h| h.min.y)
    }
}

fn framing(reason: String) -> StageError {
    StageError::Framing { reason }
}

/// Which vertices of a mesh are the head's.
#[derive(Clone, Copy)]
enum HeadRule {
    /// None of them.
    No,
    /// All of them (a built head part's head mesh).
    Whole,
    /// Those whose strongest joint is this entry of the mesh's joints (an avatar's head bone).
    Joint(u16),
}

/// One mesh to measure, with the name its errors give its owner and its head rule.
struct Measured<'a> {
    mesh: &'a PartMesh,
    owner: String,
    head: HeadRule,
}

/// The index of the strongest of a vertex's four weights; the first of equal weights wins.
fn strongest(weights: [f32; 4]) -> usize {
    (1..4).fold(0, |best, k| if weights[k] > weights[best] { k } else { best })
}

/// The measuring both kinds of body share: `joint_height` gives a humanoid joint's bind height (`None` for a bone the
/// body lacks), `stature` the body's declared height.
fn measure_meshes<'a>(
    meshes: impl Iterator<Item = Measured<'a>>,
    joint_height: impl Fn(HumanoidBone) -> Option<f32>,
    stature: Option<f32>,
) -> Result<Landmarks, StageError> {
    let height_of = |bone: HumanoidBone| -> Result<Option<f32>, StageError> {
        match joint_height(bone) {
            None => Ok(None),
            Some(h) if h.is_finite() => Ok(Some(h)),
            Some(_) => Err(framing(format!("the bind height of '{bone}' is not a finite number"))),
        }
    };
    let eyes = match (height_of(HumanoidBone::LeftEye)?, height_of(HumanoidBone::RightEye)?) {
        (Some(left), Some(right)) => Some(f32::midpoint(left, right)),
        (one, other) => one.or(other),
    };
    let chest = match height_of(HumanoidBone::UpperChest)? {
        Some(height) => Some(height),
        None => height_of(HumanoidBone::Chest)?,
    };

    let (mut body, mut head, mut bust, mut hands) = (None, None, None, None);
    for Measured { mesh, owner, head: head_rule } in meshes {
        let hand_joints = hand_joints(mesh);
        let geometry = &mesh.geometry;
        let skinned = geometry.is_skinned();
        if skinned && (geometry.joints.len() != geometry.positions.len() || geometry.weights.len() != geometry.positions.len())
        {
            return Err(framing(format!(
                "mesh '{}' of {owner} has {} vertices but {} joint sets and {} weight sets",
                mesh.name,
                geometry.positions.len(),
                geometry.joints.len(),
                geometry.weights.len()
            )));
        }
        for (i, &position) in geometry.positions.iter().enumerate() {
            if !position.is_finite() {
                return Err(framing(format!("vertex {i} of mesh '{}' of {owner} is not a finite position", mesh.name)));
            }
            Bounds::include(&mut body, position);
            let is_head = match head_rule {
                HeadRule::No => false,
                HeadRule::Whole => true,
                HeadRule::Joint(joint) => {
                    skinned && geometry.joints[i][strongest(geometry.weights[i])] == joint
                }
            };
            if is_head {
                Bounds::include(&mut head, position);
            }
            if chest.is_some_and(|c| position.y >= c) {
                Bounds::include(&mut bust, position);
            }
            if skinned && is_hand_vertex(&hand_joints, geometry.joints[i], geometry.weights[i])? {
                Bounds::include(&mut hands, position);
            }
        }
    }
    let body = body.ok_or_else(|| framing("the character has no vertex to measure".to_owned()))?;

    let height_m = stature.unwrap_or_else(|| body.height());
    // `Stature::check`'s rule: a usable height is positive and finite. A body with no stature and no extent in Y
    // (every vertex at one height) fails it too, which framing could not divide by.
    if !(height_m.is_finite() && height_m > 0.0) {
        return Err(framing(format!(
            "the height {height_m} m, from the stature or else the body's extent, is not usable: give a finite number above zero"
        )));
    }
    Ok(Landmarks { body, head, eyes, chest, bust, hands, height_m })
}

/// The part that holds the head: the first enabled one, by part id, whose kind is registered as a head.
fn head_part(built: &BuiltCharacter) -> Option<&omuzaki_char_api::PartId> {
    built.def.parts.iter().find_map(|(id, part)| {
        let registration = find_kind(part.kind.as_str())?;
        (part.enabled && (registration.meta)().category == PartCategory::Head).then_some(id)
    })
}

/// `<part>-head` in `omuzaki-char`, `head` in the mock.
fn is_head_mesh(name: &str) -> bool {
    name == "head" || name.ends_with("-head")
}

/// For each entry of `mesh.joint_bones`: whether it is a hand bone, or a finger bone under one.
fn hand_joints(mesh: &PartMesh) -> Vec<bool> {
    let is_hand = |bone: HumanoidBone| matches!(bone, HumanoidBone::LeftHand | HumanoidBone::RightHand);
    mesh.joint_bones
        .iter()
        .map(|joint| match joint {
            JointRef::Humanoid(bone) => std::iter::successors(Some(*bone), |b| b.parent()).any(is_hand),
            JointRef::Named(_) => false,
        })
        .collect()
}

/// Whether the joint with the largest weight of a vertex is a hand joint (`hand_joints` says which are). The first
/// of equal weights wins, so the answer doesn't depend on anything but the mesh.
fn is_hand_vertex(hand_joints: &[bool], joints: [u16; 4], weights: [f32; 4]) -> Result<bool, StageError> {
    let strongest = strongest(weights);
    hand_joints.get(usize::from(joints[strongest])).copied().ok_or_else(|| {
        framing(format!("a vertex is skinned to joint {} but its mesh names {} joints", joints[strongest], hand_joints.len()))
    })
}
