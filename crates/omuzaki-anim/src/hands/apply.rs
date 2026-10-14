//! Turning a [`HandShape`] into bone rotations and writing them into a pose.

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::Quat;

use super::rest::HandRest;
use super::shape::{Finger, FingerCurl, HandShape, unit};

/// Which hand.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// The character's left hand (+X in the T-pose).
    Left,
    /// The character's right hand.
    Right,
}

/// The library's rotations of a left-hand finger's three bones, base first, about the library's own axes. Fingers lie
/// along +X with the palm facing -Y and the index side toward +Z, so flexion toward the palm is a turn about -Z and a
/// swing toward the thumb a turn about -Y. The knuckle swings first and then flexes in the plane it swung into, which
/// is how the joint moves. The thumb takes the same rotations carried onto where the rig's thumb rests.
fn finger_rotations(curl: FingerCurl) -> [Quat; 3] {
    [
        Quat::from_rotation_y(-curl.spread) * Quat::from_rotation_z(-curl.mcp),
        Quat::from_rotation_z(-curl.pip),
        Quat::from_rotation_z(-curl.dip),
    ]
}

/// Reflects a left-hand rotation to the right hand, the rule of `HumanoidPose::mirrored`.
fn reflected(q: Quat) -> Quat {
    Quat::from_xyzw(q.x, -q.y, -q.z, q.w)
}

/// The rotations a finger's three bones (base first) take for `curl` on `side`, in the rig's space: the library's
/// rotations, the thumb carried onto where the rig's thumb rests, and the right hand reflected.
pub(crate) fn finger_targets(side: Side, rest: &HandRest, finger: Finger, curl: FingerCurl) -> [Quat; 3] {
    let mut rotations = finger_rotations(curl);
    if finger == Finger::Thumb {
        let thumb = rest.thumb(side);
        rotations = [0, 1, 2].map(|i| thumb[i].carry(rotations[i]));
    }
    if side == Side::Right { rotations.map(reflected) } else { rotations }
}

/// Moves one bone `weight` of the way from its current rotation to `target` (`weight` held to `0..=1`). Exact at
/// weight 1: a slerp of unit quaternions can be a ulp off, and a full-weight shape must be the shape.
pub(crate) fn set_bone(pose: &mut HumanoidPose, bone: HumanoidBone, target: Quat, weight: f32) {
    let rotation = if weight >= 1.0 { target } else { pose.get(bone).slerp(target, weight) };
    pose.set(bone, rotation);
}

/// Puts `shape` on one hand of `pose`, `weight` of the way from what the hand's 15 finger bones are now (`0`, which
/// leaves them untouched) to the shape (`1`, which sets them exactly). In between each bone is slerped, so a half-weight
/// fist is a half-curled fist. `weight` is held to `0..=1`; a NaN counts as `0`. Nothing else in `pose` changes.
///
/// `rest` is how the rig's thumbs rest, measured once when it bound ([`HandRest::of`]): the fingers rest along +X in the
/// VRM T-pose, but a thumb rests wherever the model has it, so the thumb is carried from there to where the shape wants
/// it, and the same shape reads the same on every rig. A rig whose thumbs rest at the nominal 45 degrees can pass
/// [`HandRest::nominal`].
///
/// The same call serves both hands: the right hand gets the left hand's rotations reflected through the centre plane,
/// so `apply(.., Side::Right, ..)` equals the mirror image of `apply(.., Side::Left, ..)` on a symmetric rig.
///
/// Hot path: arithmetic on fixed arrays; it never allocates or locks.
pub fn apply(pose: &mut HumanoidPose, side: Side, rest: &HandRest, shape: &HandShape, weight: f32) {
    let weight = unit(weight);
    if weight == 0.0 {
        return;
    }
    for finger in Finger::ALL {
        let targets = finger_targets(side, rest, finger, shape.curl(finger));
        for (bone, target) in finger.bones(side).into_iter().zip(targets) {
            set_bone(pose, bone, target, weight);
        }
    }
}
