//! How each hand's thumb rests, read from the rig once at bind time (RG-09).
//!
//! A character rests in the VRM T-pose (ADR 0014): fingers along +X, the palm facing -Y, every rest rotation the
//! identity. The library's finger axes rely on that. The thumb is the one part VRM does not pin down: the spec only
//! asks for it to lie somewhere forward of the fingers, so a modeller's thumb rests at 25 or 50 degrees, may rise out
//! of the palm's plane, and may bend between its bones (the template's metacarpal lies along the fingers and its free
//! thumb 30 degrees out). A thumb written for a 45 degree rest would land in the wrong place on such a rig.
//!
//! [`HandRest::of`] therefore measures each thumb bone's rest direction and keeps, per bone, the two rotations that
//! carry the library's thumb onto it: the bone's pose rotation is `enter * q * leave`, where `q` is the library's
//! rotation about its own axes and the pair turns the bone from where it rests to where the library's thumb would
//! have it. The pose path is two quaternion multiplies per thumb bone.

use std::f32::consts::FRAC_PI_4;

use omuzaki_anim_api::{HumanoidBone, Pose, Skeleton};
use omuzaki_core::glam::{Mat3, Quat, Vec3};

use super::apply::Side;
use super::shape::Finger;

/// How far forward of the fingers the library's thumb rests, radians: all three bones lie in the palm's plane at 45
/// degrees, which is how VRM models and the template are drawn. The thumb angles of a shape are rotations away from
/// that thumb.
const NOMINAL_THUMB_AZIMUTH: f32 = FRAC_PI_4;

/// The most a limb segment may lean off the T-pose's ±X axis at rest, degrees. Two segments are measured, the forearm
/// (lower arm to hand) and the palm (hand to the middle finger's first joint). The fingers themselves are not: imported
/// avatars often rest their fingers 15 to 20 degrees curled at the knuckles and are still T-poses, while a leaning arm
/// or hand changes where every finger lands. An A-pose (arms hanging near 55 degrees) is far outside this limit.
const T_POSE_TOLERANCE_DEG: f32 = 10.0;

/// The rule id of the warning for a thumb with some of its bones missing. `<area>.<rule>`, the form `omuzaki-io-api`'s
/// validation reports use, so a caller can turn it into a report finding.
pub const THUMB_PARTIAL_RULE: &str = "hands.thumb_partial";

/// The two rotations that carry the library's axes onto one thumb bone's: its pose rotation is `enter * q * leave`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct BoneFrame {
    /// Takes the library's axes to the rig's, in the frame the bone's parent left it in.
    enter: Quat,
    /// Takes the rotation back out of the bone's own measured frame.
    leave: Quat,
}

impl BoneFrame {
    /// The rotation `q` of the library, as it turns this bone.
    pub(super) fn carry(self, q: Quat) -> Quat {
        self.enter * q * self.leave
    }
}

/// The library's own thumb: three bones at 45 degrees, `frame * q * frame^-1` for the finger axes turned to the thumb.
fn nominal_thumb() -> [BoneFrame; 3] {
    let frame = Quat::from_rotation_y(-NOMINAL_THUMB_AZIMUTH);
    [BoneFrame { enter: frame, leave: frame.inverse() }; 3]
}

/// How the thumbs of one rig rest, as the hand-shape library needs to know it. Measure once when the rig binds
/// ([`HandRest::of`]) and pass it to every [`apply`](super::apply) call: the pose path then reads it and does
/// nothing else. `Copy`, fixed-size and read-only in the pose path.
///
/// **What is measured.** The rest geometry of the skeleton in skeleton space, which is what a normalised pose rotates
/// (`omuzaki-rig`'s `pose` module: a rotation turns a bone about the rest axes, whatever the bone's own rest rotation
/// is). For each thumb bone, its direction (to its child; the fingertip bone, which has none in the skeleton, is taken
/// to continue the bone before it) gives an azimuth, the angle in the palm's plane from the fingers (positive toward the
/// thumb side), and an elevation, the angle out of that plane (positive toward the palm). Each bone is then turned
/// from that direction to the one the library's nominal thumb has after the same shape, so a thumb that rests at 30
/// degrees, rises out of the palm or bends between its bones still ends where the shape puts it, and a shape reads the
/// same on every rig. A rig whose thumb rests at the nominal 45 degrees gets the library's rotations unchanged. The right
/// hand is measured as the left hand reflected through the centre plane, the rule `apply` uses for rotations.
///
/// A rig without thumb bones (VRM makes them optional) has nothing to measure and gets [`HandRest::nominal`]. A thumb
/// with only some of its bones is a broken rig: it also gets the nominal thumb, and [`HandRest::warnings`] says so.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HandRest {
    left: [BoneFrame; 3],
    right: [BoneFrame; 3],
    partial_left: bool,
    partial_right: bool,
}

/// A finding about a rig's hands that the measuring got past: the rest it gives is still usable, but the author should
/// look at it. The caller shows it in the report card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandRestWarning {
    /// Stable rule id, `<area>.<rule>` (see [`THUMB_PARTIAL_RULE`]).
    pub rule: &'static str,
    /// Which hand.
    pub side: Side,
    /// What is wrong, in a sentence a person can read.
    pub message: String,
}

/// Why the rest of a rig's hands could not be measured.
#[derive(Copy, Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum HandRestError {
    /// The skeleton has no humanoid roles, so there are no hands to find.
    #[error("the skeleton is not a humanoid, so it has no hands")]
    NotHumanoid,
    /// A limb segment (the forearm, or the palm) points off the T-pose's ±X axis. Characters rest in the VRM T-pose
    /// (ADR 0014); a rig resting in another pose would pose every hand wrong.
    #[error(
        "{bone} points {degrees:.1} degrees off the T-pose's x axis (the limit is 10): the rig must rest in the VRM T-pose (ADR 0014), not in another pose"
    )]
    NotTPose {
        /// The bone the off-axis segment starts from: the lower arm for the forearm, the hand for the palm.
        bone: HumanoidBone,
        /// How far it leans off the nearest of ±X, degrees.
        degrees: f32,
    },
    /// Two joints of a thumb coincide, are not finite, or the thumb points along the palm's normal, so there is no
    /// direction to measure.
    #[error(
        "the {side:?} thumb has no measurable direction: its rest joints coincide, are not finite, or point out of the palm"
    )]
    Degenerate {
        /// Which hand.
        side: Side,
    },
}

impl HandRest {
    /// The thumb the library is written for, on both sides: three bones at 45 degrees in the palm's plane. Also the
    /// rest of a rig that has no thumbs to measure.
    pub fn nominal() -> Self {
        Self { left: nominal_thumb(), right: nominal_thumb(), partial_left: false, partial_right: false }
    }

    /// Measures both thumbs of `skeleton` from its rest pose. Cold: it runs forward kinematics once. Call it when the
    /// rig binds and keep the result with the rig.
    ///
    /// # Errors
    /// [`HandRestError::NotHumanoid`] for a generic skeleton; [`HandRestError::NotTPose`] when a finger does not rest
    /// along the T-pose's ±X axis; [`HandRestError::Degenerate`] when a thumb's joints give no direction. The caller
    /// decides what to do with an error; there is no silent fallback.
    pub fn of(skeleton: &Skeleton) -> Result<Self, HandRestError> {
        skeleton.require_humanoid().map_err(|_| HandRestError::NotHumanoid)?;
        let mut world = Pose::IDENTITY;
        skeleton.world_pose(&skeleton.rest_pose(), &mut world);
        let joint = |bone: HumanoidBone| skeleton.humanoid_bone(bone).map(|id| world.local[id.index()].translation);
        check_t_pose(&joint)?;
        let (left, right) = (measure(&joint, Side::Left)?, measure(&joint, Side::Right)?);
        Ok(Self { left: left.frames, right: right.frames, partial_left: left.partial, partial_right: right.partial })
    }

    /// The findings of the measuring that did not stop it: one [`THUMB_PARTIAL_RULE`] warning per hand whose thumb had
    /// some of its bones but not all (that hand is measured with the nominal thumb). Empty for a complete rig and for a
    /// rig without thumbs. Cold: it allocates, so call it at bind time, never in the pose path.
    pub fn warnings(&self) -> Vec<HandRestWarning> {
        [Side::Left, Side::Right]
            .into_iter()
            .filter(|&side| self.partial(side))
            .map(|side| HandRestWarning {
                rule: THUMB_PARTIAL_RULE,
                side,
                message: format!(
                    "the {side:?} thumb has only some of its three bones (metacarpal, proximal, distal): the nominal thumb is used for that hand"
                ),
            })
            .collect()
    }

    fn partial(&self, side: Side) -> bool {
        match side {
            Side::Left => self.partial_left,
            Side::Right => self.partial_right,
        }
    }

    /// The frames of the thumb of one hand, base bone first.
    pub(super) fn thumb(&self, side: Side) -> [BoneFrame; 3] {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }
}

/// The rotation whose first axis is `along` and whose second is the back of the hand (+Y in the T-pose) made square to
/// it: the library's finger axes turned to point down a bone. `None` if `along` points along the back of the hand.
fn frame_along(along: Vec3) -> Option<Quat> {
    let back = (Vec3::Y - along * along.dot(Vec3::Y)).try_normalize()?;
    Some(Quat::from_mat3(&Mat3::from_cols(along, back, along.cross(back))))
}

/// Refuses a rig whose forearm or palm does not run along the T-pose's ±X axis at rest. Per side, two segments are
/// measured: lower arm to hand (the forearm) and hand to the middle finger's first joint (the palm's long axis). A
/// segment whose joints are missing or coincide has no direction to read and is skipped; the thumb's own measuring
/// reports a degenerate thumb. Finger curl is deliberately not measured (see [`T_POSE_TOLERANCE_DEG`]).
fn check_t_pose(joint: &impl Fn(HumanoidBone) -> Option<Vec3>) -> Result<(), HandRestError> {
    for side in [Side::Left, Side::Right] {
        let (forearm, hand) = match side {
            Side::Left => (HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand),
            Side::Right => (HumanoidBone::RightLowerArm, HumanoidBone::RightHand),
        };
        let palm_end = Finger::Middle.bones(side)[0];
        for (from, to) in [(forearm, hand), (hand, palm_end)] {
            let (Some(a), Some(b)) = (joint(from), joint(to)) else { continue };
            let Some(direction) = (b - a).try_normalize() else { continue };
            let degrees = direction.x.abs().min(1.0).acos().to_degrees();
            if degrees > T_POSE_TOLERANCE_DEG {
                return Err(HandRestError::NotTPose { bone: from, degrees });
            }
        }
    }
    Ok(())
}

/// One hand's measured thumb: its frames, and whether some of its bones were missing so the nominal thumb stands in.
struct MeasuredThumb {
    frames: [BoneFrame; 3],
    partial: bool,
}

/// Measures the thumb of `side`, in the left-hand convention (a right thumb is reflected first).
fn measure(joint: &impl Fn(HumanoidBone) -> Option<Vec3>, side: Side) -> Result<MeasuredThumb, HandRestError> {
    let place = |p: Vec3| if side == Side::Right { Vec3::new(-p.x, p.y, p.z) } else { p };
    let found = Finger::Thumb.bones(side).map(|bone| joint(bone).map(place));
    let [Some(a), Some(b), Some(c)] = found else {
        // VRM makes thumbs optional, so a thumb with none of its bones is simply absent. With some but not all, the rig
        // is broken: the nominal thumb stands in and the caller is warned.
        return Ok(MeasuredThumb { frames: nominal_thumb(), partial: found.iter().any(Option::is_some) });
    };
    let degenerate = || HandRestError::Degenerate { side };
    let first = (b - a).try_normalize().ok_or_else(degenerate)?;
    let second = (c - b).try_normalize().ok_or_else(degenerate)?;
    // The tip bone's direction is not in the skeleton; it is taken to continue the bone before it.
    let measured = [frame_along(first), frame_along(second), frame_along(second)];
    let [Some(m0), Some(m1), Some(m2)] = measured else { return Err(degenerate()) };
    // Bone `i` is carried from its own measured frame to the nominal one, entering from the frame its parent left it
    // in: the nominal thumb for the first bone, the parent's measured frame after.
    let (measured, entering) = ([m0, m1, m2], [Quat::from_rotation_y(-NOMINAL_THUMB_AZIMUTH), m0, m1]);
    let frames = [0, 1, 2].map(|i| BoneFrame { enter: entering[i], leave: measured[i].inverse() });
    Ok(MeasuredThumb { frames, partial: false })
}
