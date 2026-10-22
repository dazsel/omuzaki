//! [`derive_anchors`]: an imported avatar's attachment points, from its skeleton alone (design D1, condition 2).
//!
//! A kit part attaches to a bone or to an anchor another part published ([`Attach`](omuzaki_char_api::Attach)). An
//! imported avatar has no head part to publish the head's top, so its anchors are derived here, from the skeleton and
//! the pose its meshes were made in, and the stage publishes them as the part [`AVATAR_PART`] for the attachments to
//! build against. Every anchor is in bind space, the space of the avatar's meshes, with the rotation of its bone in the
//! normalised space of [`HumanoidPose`] (the identity in the T-pose), which is the frame the kit gives a bone it attaches
//! to.
//!
//! - **One anchor per humanoid bone present**, at its joint, named by [`bone_anchor`] (`left-upper-arm`).
//! - **`head-top`** ([`HEAD_TOP`]), the crown. The kit's head publishes its `top` at the cranium's top, which an
//!   imported skeleton does not have. In every body template the kit ships (mini, soft-sd, standard and tall) that top
//!   sits [`TOP_OVER_EYES`] times the eyes' height above the head joint, and [`TOP_BACK`] of that rise behind it, so the
//!   same rule is applied to the avatar's eye bones. A skeleton without eye bones (they are optional in VRM) takes the
//!   standard adult template's ratio of the rise to the head joint's height over the feet ([`TOP_OVER_STANDING`]).
//!   Once the meshes are in, [`avatar_anchors`] moves it to the model's own crown ([`crown`]), which knows the head's
//!   real size; the skeleton rule is what stands when no mesh follows the head.
//! - **`left-grip` and `right-grip`** ([`LEFT_GRIP`], [`RIGHT_GRIP`]), where a held prop's handle sits. The kit publishes
//!   no grip of its own, so this is the palm's centre: halfway from the wrist to the middle finger's root, or, on a hand
//!   with no finger bones, [`GRIP_PAST_WRIST`] of the forearm's length beyond the wrist.

use std::collections::BTreeMap;

use omuzaki_anim_api::{BoneId, HumanoidBone, HumanoidFrames, HumanoidPose, Pose, Skeleton, Transform};
use omuzaki_char_api::{Anchor, AnchorId, JointRef};
use omuzaki_core::glam::{Mat4, Quat, Vec3};

use crate::avatar::ImportedAvatar;

/// The part id the avatar's own anchors are published under, the same reserved id as its licence row (`sources/avatar`).
pub const AVATAR_PART: &str = "avatar";

/// The crown of the head.
pub const HEAD_TOP: &str = "head-top";

/// The left palm, where a held prop's handle sits.
pub const LEFT_GRIP: &str = "left-grip";

/// The right palm.
pub const RIGHT_GRIP: &str = "right-grip";

/// The head-top's rise over the head joint, in eye heights over it: 3.12 in each of the kit's four templates.
pub const TOP_OVER_EYES: f32 = 3.12;

/// How far behind the head joint the head-top sits, as a fraction of its rise: 0.049 in each of the kit's templates.
pub const TOP_BACK: f32 = 0.049;

/// The head-top's rise over the head joint as a fraction of the head joint's height over the lower foot, when the
/// skeleton has no eyes: the standard adult template's 0.23.
pub const TOP_OVER_STANDING: f32 = 0.23;

/// How close to the head's vertical centre line the crown is looked for, as a fraction of the head's half-width: wide
/// enough for the top of the hair, narrow enough to leave out ears and side buns.
pub const CROWN_AXIS: f32 = 0.35;

/// How far beyond the wrist a grip sits on a hand without finger bones, as a fraction of the forearm's length.
pub const GRIP_PAST_WRIST: f32 = 0.2;

/// The anchor name of a humanoid bone: its VRM name in kebab case (`leftUpperArm` is `left-upper-arm`), always a valid
/// [`AnchorId`].
pub fn bone_anchor(bone: HumanoidBone) -> String {
    let mut id = String::with_capacity(bone.name().len() + 4);
    for c in bone.name().chars() {
        if c.is_ascii_uppercase() {
            id.push('-');
        }
        id.push(c.to_ascii_lowercase());
    }
    id
}

/// Adds `anchor` as `name`. Every name here is a valid slug (the constants, and [`bone_anchor`]'s kebab-case VRM
/// names), so nothing is ever skipped.
fn put(anchors: &mut BTreeMap<AnchorId, Anchor>, name: &str, anchor: Anchor) {
    if let Ok(id) = AnchorId::new(name) {
        anchors.insert(id, anchor);
    }
}

/// Where each bone of `skeleton` is with the humanoid bones at `bind`: bind-space transforms, by joint. `None` only for
/// a skeleton whose bone list and humanoid table disagree, which a valid skeleton never does.
pub fn bind_world(skeleton: &Skeleton, bind: &HumanoidPose) -> Option<Pose> {
    let rest: Vec<Transform> = skeleton.bones().iter().map(|b| b.rest).collect();
    let mut local = Pose::IDENTITY;
    HumanoidFrames::new(skeleton).apply_humanoid(&rest, bind, &mut local).ok()?;
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&local, &mut world);
    Some(world)
}

/// The bind-space matrix of each bone of `skeleton` at `bind`, by joint: what a mesh bound there inverts for its
/// inverse binds. Empty for a skeleton [`bind_world`] refuses.
pub fn bind_matrices(skeleton: &Skeleton, bind: &HumanoidPose) -> Vec<Mat4> {
    bind_world(skeleton, bind)
        .map(|world| world.local.iter().take(skeleton.len()).map(|t| t.to_mat4()).collect())
        .unwrap_or_default()
}

/// One anchor per humanoid bone `skeleton` has, plus [`HEAD_TOP`], [`LEFT_GRIP`] and [`RIGHT_GRIP`] (each only when
/// its bones are there), placed as the module docs say, with the bones at `bind`. Empty for a generic skeleton.
pub fn derive_anchors(skeleton: &Skeleton, bind: &HumanoidPose) -> BTreeMap<AnchorId, Anchor> {
    let mut anchors = BTreeMap::new();
    let (Some(map), Some(frames)) = (skeleton.humanoid_map(), frames(skeleton, bind)) else { return anchors };
    let frame = |bone: HumanoidBone| map.get(bone).map(|id| frames.frame(id.index()));
    for (bone, _) in map.iter() {
        if let Some((position, rotation)) = frame(bone) {
            put(&mut anchors, &bone_anchor(bone), Anchor { bone: JointRef::Humanoid(bone), position, rotation });
        }
    }
    if let Some(top) = head_top(&frame) {
        put(&mut anchors, HEAD_TOP, top);
    }
    let sides = [
        (LEFT_GRIP, HumanoidBone::LeftHand, HumanoidBone::LeftMiddleProximal, HumanoidBone::LeftLowerArm),
        (RIGHT_GRIP, HumanoidBone::RightHand, HumanoidBone::RightMiddleProximal, HumanoidBone::RightLowerArm),
    ];
    for (name, hand, finger, forearm) in sides {
        if let Some(grip) = grip(&frame, hand, finger, forearm) {
            put(&mut anchors, name, grip);
        }
    }
    anchors
}

/// The anchors of a prepared avatar: [`derive_anchors`], with `head-top` moved to the model's own crown when its
/// meshes show one ([`crown`]). The importer fills [`ImportedAvatar::anchors`] with this once the meshes are in.
pub fn avatar_anchors(avatar: &ImportedAvatar) -> BTreeMap<AnchorId, Anchor> {
    let mut anchors = derive_anchors(&avatar.skeleton, &avatar.bind);
    if let Some(top) = crown(avatar) {
        put(&mut anchors, HEAD_TOP, top);
    }
    anchors
}

/// The model's crown: the highest point of what the head carries (vertices weighted at least half to the head or a
/// bone under it, hair included) within [`CROWN_AXIS`] of the head's half-width from its vertical centre line, in the
/// head's frame at the bind pose. A rule from the skeleton alone cannot know how big a head is (a chibi's is half its
/// height); the mesh does. `None` when no skinned vertex follows the head.
pub fn crown(avatar: &ImportedAvatar) -> Option<Anchor> {
    let skeleton = &avatar.skeleton;
    let head = skeleton.humanoid_bone(HumanoidBone::Head)?;
    let bind = bind_matrices(skeleton, &avatar.bind);
    let (pivot, turn) = frames(skeleton, &avatar.bind)?.frame(head.index());
    let under_head: Vec<bool> = (0..skeleton.len())
        .map(|i| u16::try_from(i).is_ok_and(|i| skeleton.is_ancestor_or_self(head, BoneId(i))))
        .collect();
    let to_head = Mat4::from_quat(turn.conjugate()) * Mat4::from_translation(-pivot);
    let mut carried = Vec::new();
    for mesh in &avatar.meshes {
        let joints: Vec<Option<(usize, Mat4)>> = mesh
            .mesh
            .joint_bones
            .iter()
            .zip(&mesh.inverse_bind)
            .map(|(joint, inverse)| {
                let bone = match joint {
                    JointRef::Humanoid(bone) => skeleton.humanoid_bone(*bone),
                    JointRef::Named(name) => skeleton.find(name.as_str()),
                }?;
                bind.get(bone.index()).map(|m| (bone.index(), to_head * *m * *inverse))
            })
            .collect();
        let geometry = &mesh.mesh.geometry;
        for ((v, ix), w) in geometry.positions.iter().zip(&geometry.joints).zip(&geometry.weights) {
            let mut on_head = 0.0;
            let mut at = Vec3::ZERO;
            for (j, weight) in ix.iter().zip(w) {
                if let Some(Some((bone, m))) = joints.get(usize::from(*j)) {
                    at += m.transform_point3(*v) * *weight;
                    on_head += if under_head[*bone] { *weight } else { 0.0 };
                }
            }
            if on_head >= 0.5 {
                carried.push(at);
            }
        }
    }
    let (lo, hi) = carried.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let (mid, reach) = ((lo + hi) * 0.5, (hi.x - lo.x) * 0.5 * CROWN_AXIS);
    let rise = carried
        .iter()
        .filter(|p| (p.x - mid.x).hypot(p.z - mid.z) <= reach)
        .map(|p| p.y)
        .reduce(f32::max)
        .filter(|rise| *rise > 0.0)?;
    let position = pivot + turn * Vec3::new(mid.x, rise, mid.z);
    Some(Anchor { bone: JointRef::Humanoid(HumanoidBone::Head), position, rotation: turn })
}

/// The bind-pose and rest world transforms of a skeleton's bones, for the normalised frame of each.
struct Frames {
    world: Pose,
    rest: Pose,
}

impl Frames {
    /// Joint `id`'s position and normalised rotation: its bind rotation with its own rest axes taken out (`D = W·Wb⁻¹`).
    fn frame(&self, id: usize) -> (Vec3, Quat) {
        let (at, axes) = (self.world.local[id], self.rest.local[id].rotation);
        (at.translation, (at.rotation * axes.conjugate()).normalize())
    }
}

fn frames(skeleton: &Skeleton, bind: &HumanoidPose) -> Option<Frames> {
    let world = bind_world(skeleton, bind)?;
    let mut rest = Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut rest);
    Some(Frames { world, rest })
}

/// The crown, as [`TOP_OVER_EYES`] and [`TOP_BACK`] place it in the head's frame.
fn head_top(frame: &impl Fn(HumanoidBone) -> Option<(Vec3, Quat)>) -> Option<Anchor> {
    let (head, turn) = frame(HumanoidBone::Head)?;
    let eyes = frame(HumanoidBone::LeftEye).zip(frame(HumanoidBone::RightEye)).map(|((l, _), (r, _))| (l + r) * 0.5);
    let over_eyes = eyes.map(|eyes| (turn.conjugate() * (eyes - head)).y * TOP_OVER_EYES).filter(|rise| *rise > 0.0);
    let rise = over_eyes.or_else(|| {
        let feet = [HumanoidBone::LeftFoot, HumanoidBone::RightFoot].map(|b| frame(b).map(|(at, _)| at.y));
        let lowest = feet.into_iter().flatten().reduce(f32::min)?;
        Some((head.y - lowest) * TOP_OVER_STANDING).filter(|rise| *rise > 0.0)
    })?;
    let position = head + turn * Vec3::new(0.0, rise, -TOP_BACK * rise);
    Some(Anchor { bone: JointRef::Humanoid(HumanoidBone::Head), position, rotation: turn })
}

/// The palm's centre of `hand`.
fn grip(
    frame: &impl Fn(HumanoidBone) -> Option<(Vec3, Quat)>,
    hand: HumanoidBone,
    finger: HumanoidBone,
    forearm: HumanoidBone,
) -> Option<Anchor> {
    let (wrist, turn) = frame(hand)?;
    let position = if let Some((root, _)) = frame(finger) {
        (wrist + root) * 0.5
    } else {
        let (elbow, _) = frame(forearm)?;
        wrist + (wrist - elbow) * GRIP_PAST_WRIST
    };
    Some(Anchor { bone: JointRef::Humanoid(hand), position, rotation: turn })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A humanoid at rest with its joints where the kit's standard adult template puts them (eyes, head, hands, middle
    /// fingers and feet as measured; the rest between), so the rules can be checked against the kit's own head-top.
    fn standard(eyes: bool, fingers: bool) -> Skeleton {
        use HumanoidBone as B;
        let at = |bone: B| -> Vec3 {
            let mirror = if bone.name().starts_with("left") { 1.0 } else { -1.0 };
            let side = |x: f32, y: f32, z: f32| Vec3::new(mirror * x, y, z);
            match bone {
                B::Hips => Vec3::new(0.0, 0.95, 0.0),
                B::Spine => Vec3::new(0.0, 1.05, 0.0),
                B::Chest => Vec3::new(0.0, 1.18, 0.0),
                B::Neck => Vec3::new(0.0, 1.327_988_4, 0.0),
                B::Head => Vec3::new(0.0, 1.366_721_4, 0.0),
                B::LeftEye | B::RightEye => side(0.063_567_77, 1.460_628_4, 0.074_370_06),
                B::LeftUpperArm | B::RightUpperArm => side(0.17, 1.28, 0.0),
                B::LeftLowerArm | B::RightLowerArm => side(0.273_897_6, 1.062_556_7, -0.008_299_929),
                B::LeftHand | B::RightHand => side(0.363_647_5, 0.878_547_25, 0.008_299_933),
                B::LeftMiddleProximal | B::RightMiddleProximal => side(0.402_588_34, 0.797_176_6, 0.025_478_855),
                B::LeftUpperLeg | B::RightUpperLeg => side(0.09, 0.9, 0.0),
                B::LeftLowerLeg | B::RightLowerLeg => side(0.08, 0.5, 0.0),
                _ => side(0.063_632_78, 0.099_599_12, 0.0),
            }
        };
        let mut bones = vec![
            B::Hips,
            B::Spine,
            B::Chest,
            B::Neck,
            B::Head,
            B::LeftUpperArm,
            B::LeftLowerArm,
            B::LeftHand,
            B::RightUpperArm,
            B::RightLowerArm,
            B::RightHand,
            B::LeftUpperLeg,
            B::LeftLowerLeg,
            B::LeftFoot,
            B::RightUpperLeg,
            B::RightLowerLeg,
            B::RightFoot,
        ];
        if eyes {
            bones.extend([B::LeftEye, B::RightEye]);
        }
        if fingers {
            bones.extend([B::LeftMiddleProximal, B::RightMiddleProximal]);
        }
        let parent = |bone: B| std::iter::successors(bone.parent(), |p| p.parent()).find(|p| bones.contains(p));
        Skeleton::humanoid(&bones, |bone| Transform::from_translation(at(bone) - parent(bone).map_or(Vec3::ZERO, at)))
            .expect("a valid humanoid")
    }

    fn get(anchors: &BTreeMap<AnchorId, Anchor>, name: &str) -> Anchor {
        anchors.get(&AnchorId::new(name).expect("a slug")).cloned().unwrap_or_else(|| panic!("no anchor {name}"))
    }

    #[test]
    fn the_head_top_from_the_eyes_lands_on_the_kits_own_within_a_millimetre() {
        let anchors = derive_anchors(&standard(true, true), &HumanoidPose::REST);
        let top = get(&anchors, HEAD_TOP);
        // The standard template's head publishes its top at (0, 1.66, -0.0144).
        assert!(top.position.distance(Vec3::new(0.0, 1.660_000_1, -0.014_447_219)) < 1e-3, "{top:?}");
        assert_eq!((top.bone, top.rotation), (JointRef::Humanoid(HumanoidBone::Head), Quat::IDENTITY));
        // Without eyes it falls back to the standing height, which for this template lands within a centimetre.
        let blind = get(&derive_anchors(&standard(false, true), &HumanoidPose::REST), HEAD_TOP);
        assert!((blind.position.y - 1.66).abs() < 0.01, "{blind:?}");
    }

    #[test]
    fn a_grip_is_the_palms_centre_or_past_the_wrist_without_fingers() {
        let anchors = derive_anchors(&standard(true, true), &HumanoidPose::REST);
        let (grip, hand, finger) =
            (get(&anchors, LEFT_GRIP), get(&anchors, "left-hand"), get(&anchors, "left-middle-proximal"));
        assert!(grip.position.distance((hand.position + finger.position) * 0.5) < 1e-6);
        assert_eq!(grip.bone, JointRef::Humanoid(HumanoidBone::LeftHand));
        let anchors = derive_anchors(&standard(true, false), &HumanoidPose::REST);
        let (grip, hand, elbow) =
            (get(&anchors, RIGHT_GRIP), get(&anchors, "right-hand"), get(&anchors, "right-lower-arm"));
        assert!(grip.position.distance(hand.position + (hand.position - elbow.position) * GRIP_PAST_WRIST) < 1e-6);
    }

    #[test]
    fn every_bone_present_has_an_anchor_at_its_joint_that_turns_with_the_bind() {
        let skeleton = standard(true, true);
        let mut bind = HumanoidPose::REST;
        let lowered = Quat::from_rotation_z(-0.6);
        bind.set(HumanoidBone::LeftUpperArm, lowered);
        let anchors = derive_anchors(&skeleton, &bind);
        let map = skeleton.humanoid_map().expect("a humanoid");
        assert_eq!(anchors.len(), map.iter().count() + 3, "one per bone, the head-top and two grips");
        let hand = get(&anchors, "left-hand");
        assert!(hand.rotation.abs_diff_eq(lowered, 1e-6), "the hand hangs with the lowered arm: {hand:?}");
        let shoulder = get(&anchors, "left-upper-arm").position;
        let expected = shoulder + lowered * (Vec3::new(0.363_647_5, 0.878_547_25, 0.008_299_933) - shoulder);
        assert!(hand.position.abs_diff_eq(expected, 1e-5), "{hand:?} vs {expected}");
    }

    #[test]
    fn a_bone_anchor_is_its_vrm_name_in_kebab_case() {
        assert_eq!(bone_anchor(HumanoidBone::LeftUpperArm), "left-upper-arm");
        assert_eq!(bone_anchor(HumanoidBone::Head), "head");
        assert_eq!(bone_anchor(HumanoidBone::RightThumbMetacarpal), "right-thumb-metacarpal");
        for bone in HumanoidBone::ALL {
            assert!(AnchorId::new(bone_anchor(bone)).is_ok(), "{bone}");
        }
    }
}
