//! Reaches to hand goals (#2414): each [`Reach`] of a frame turned into a root-space [`Limb`] for the two-bone solve,
//! on this skeleton's posed bones (see [`omuzaki_anim_api::reach`] for the reach frame).
//!
//! The rig runs this after the humanoid stage, on the world pose of the animated body, so a reach on the chest follows
//! the chest as the clip or the tracker leans it. A hand goal the caller set itself wins over a reach for that hand:
//! a goal is the scene's say (a prop, another member's hand), a reach the clip's.
//!
//! No allocation and no state: a pure function of its inputs.

use omuzaki_anim_api::{HumanoidBone, HumanoidMap, HumanoidPose, IkGoals, Limb, Pose, Reach, Reaches};
use omuzaki_core::glam::{Quat, Vec3};

/// Arms shorter than this (metres, shoulder to wrist) have no length to scale a reach by: the reach is skipped.
const MIN_ARM: f32 = 1e-3;

/// The bone's rotation in the normalised pose: its own and every ancestor's, root first. The axes of the reach frame.
pub fn normalised_rotation(pose: &HumanoidPose, bone: HumanoidBone) -> Quat {
    let mut rotation = pose.get(bone);
    let mut at = bone;
    while let Some(parent) = at.parent() {
        rotation = pose.get(parent) * rotation;
        at = parent;
    }
    rotation.normalize()
}

/// `goals` with each hand that has no goal of its own (or one of weight zero) given its reach, resolved on `world`
/// (the root-relative transforms of the animated body on a skeleton with `map`). A reach whose bone (and every
/// ancestor of it) or whose arm the skeleton lacks is skipped.
pub fn resolve(map: &HumanoidMap, world: &Pose, pose: &HumanoidPose, reaches: &Reaches, goals: &IkGoals) -> IkGoals {
    let reaches = reaches.finite_only();
    let mut out = *goals;
    let free = |goal: &Option<Limb>| goal.is_none_or(|g| g.weight <= 0.0);
    if free(&goals.left_hand)
        && let Some(goal) = reaches.left.and_then(|r| limb(map, world, pose, r, Side::Left))
    {
        out.left_hand = Some(goal);
    }
    if free(&goals.right_hand)
        && let Some(goal) = reaches.right.and_then(|r| limb(map, world, pose, r, Side::Right))
    {
        out.right_hand = Some(goal);
    }
    out
}

/// Which arm reaches.
#[derive(Copy, Clone)]
enum Side {
    Left,
    Right,
}

impl Side {
    /// `[upper arm, lower arm, hand]`.
    const fn arm(self) -> [HumanoidBone; 3] {
        match self {
            Self::Left => [HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand],
            Self::Right => [HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm, HumanoidBone::RightHand],
        }
    }
}

/// The root-space position of `bone`, or of its nearest ancestor the skeleton has.
fn joint(map: &HumanoidMap, world: &Pose, bone: HumanoidBone) -> Option<Vec3> {
    let mut at = Some(bone);
    while let Some(bone) = at {
        if let Some(id) = map.get(bone) {
            return world.local.get(id.index()).map(|t| t.translation);
        }
        at = bone.parent();
    }
    None
}

/// The hand goal of `reach` for `side`.
fn limb(map: &HumanoidMap, world: &Pose, pose: &HumanoidPose, reach: Reach, side: Side) -> Option<Limb> {
    if reach.weight <= 0.0 {
        return None;
    }
    let [upper, lower, hand] = side.arm();
    let at = |bone| map.get(bone).and_then(|id| world.local.get(id.index())).map(|t| t.translation);
    let (shoulder, elbow, wrist) = (at(upper)?, at(lower)?, at(hand)?);
    let arm = shoulder.distance(elbow) + elbow.distance(wrist);
    if !(arm.is_finite() && arm > MIN_ARM) {
        return None;
    }
    let axes = normalised_rotation(pose, reach.bone);
    let target = joint(map, world, reach.bone)? + axes * (reach.offset * arm);
    let bend = shoulder + axes * (reach.pole.normalize_or_zero() * arm);
    let goal = Limb { target, pole: bend, weight: reach.weight.clamp(0.0, 1.0) };
    goal.is_finite().then_some(goal)
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use omuzaki_anim_api::{Skeleton, Transform};

    use super::*;

    /// A spine with one arm on each side, T-posed: hips at 1 m, the upper chest 0.3 m above (no chest or neck bone),
    /// shoulders 0.2 m out, upper arm and forearm 0.25 m each.
    fn body() -> (Skeleton, Pose) {
        use HumanoidBone as B;
        let bones = [
            B::Hips,
            B::Spine,
            B::UpperChest,
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
        let rest = |bone: B| {
            let (x, y) = match bone {
                B::Hips => (0.0, 1.0),
                B::Spine => (0.0, 0.1),
                B::UpperChest => (0.0, 0.2),
                B::Head => (0.0, 0.25),
                B::LeftUpperArm => (0.2, 0.0),
                B::RightUpperArm => (-0.2, 0.0),
                B::LeftLowerArm | B::LeftHand => (0.25, 0.0),
                B::RightLowerArm | B::RightHand => (-0.25, 0.0),
                B::LeftUpperLeg => (0.1, 0.0),
                B::RightUpperLeg => (-0.1, 0.0),
                _ => (0.0, -0.45),
            };
            Transform::from_translation(Vec3::new(x, y, 0.0))
        };
        let skeleton = Skeleton::humanoid(&bones, rest).unwrap();
        let mut world = Pose::IDENTITY;
        skeleton.world_pose(&skeleton.rest_pose(), &mut world);
        (skeleton, world)
    }

    fn chest(offset: Vec3) -> Reach {
        Reach { bone: HumanoidBone::UpperChest, offset, pole: Vec3::NEG_Y, weight: 1.0 }
    }

    #[test]
    fn a_reach_lands_on_its_bone_in_arm_lengths_and_turns_with_it() {
        let (skeleton, world) = body();
        let map = skeleton.humanoid_map().unwrap();
        let reaches = Reaches { right: Some(chest(Vec3::new(0.0, 0.0, 0.4))), ..Reaches::NONE };
        let goals = resolve(map, &world, &HumanoidPose::REST, &reaches, &IkGoals::NONE);
        assert!(goals.left_hand.is_none());
        let right = goals.right_hand.unwrap();
        assert!(right.target.abs_diff_eq(Vec3::new(0.0, 1.3, 0.2), 1e-5), "0.4 arm lengths of 0.5 m ahead: {right:?}");
        assert!(right.pole.y < 1.3, "the elbow points down");

        // The chest's normalised rotation turns the frame: a quarter turn about Y puts "forward" at the left.
        let mut turned = HumanoidPose::REST;
        turned.set(HumanoidBone::Spine, Quat::from_rotation_y(FRAC_PI_2));
        let goals = resolve(map, &world, &turned, &reaches, &IkGoals::NONE);
        assert!(goals.right_hand.unwrap().target.abs_diff_eq(Vec3::new(0.2, 1.3, 0.0), 1e-5));
    }

    #[test]
    fn a_goal_of_the_callers_wins_and_bad_reaches_are_skipped() {
        let (skeleton, world) = body();
        let map = skeleton.humanoid_map().unwrap();
        let own = Limb { target: Vec3::ONE, pole: Vec3::Z, weight: 1.0 };
        let goals = IkGoals { left_hand: Some(own), ..IkGoals::NONE };
        let reaches =
            Reaches { left: Some(chest(Vec3::Z).mirrored()), right: Some(chest(Vec3::new(f32::NAN, 0.0, 0.0))) };
        let out = resolve(map, &world, &HumanoidPose::REST, &reaches, &goals);
        assert_eq!(out.left_hand, Some(own));
        assert_eq!(out.right_hand, None);
        let weightless = Reaches { right: Some(Reach { weight: 0.0, ..chest(Vec3::Z) }), ..Reaches::NONE };
        assert_eq!(resolve(map, &world, &HumanoidPose::REST, &weightless, &IkGoals::NONE), IkGoals::NONE);
    }

    #[test]
    fn a_missing_bone_falls_back_to_its_nearest_ancestor() {
        let (skeleton, world) = body();
        let map = skeleton.humanoid_map().unwrap();
        // The test body has no chest bone (only an upper chest), so a reach on the neck uses the upper chest's joint.
        let neck = Reach { bone: HumanoidBone::Neck, ..chest(Vec3::ZERO) };
        let goals =
            resolve(map, &world, &HumanoidPose::REST, &Reaches { right: Some(neck), ..Reaches::NONE }, &IkGoals::NONE);
        assert!(goals.right_hand.unwrap().target.abs_diff_eq(Vec3::new(0.0, 1.3, 0.0), 1e-5));
    }
}
