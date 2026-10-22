//! The pose an imported avatar stands in when nothing animates its arms: at ease (`docs/design/byom.md` D4).
//!
//! VRM files rest in the T-pose (VRM 1.0 `tpose.md`), and an avatar is bound in the pose its file has (ADR 0014). Shown
//! as authored, a `VRoid` or `UniVRM` model would perform live with its arms straight out, which no performer wants on a
//! stream; `VSeeFace` and `VTube Studio` lower the arms of such a model for the same reason. So an arm whose bind lies
//! within [`T_ARM_DEG`] of the horizontal is lowered to hang [`HANG_DEG`] below it, as the kit's own sculpted A-pose
//! hangs, with the elbow bent a little forward. An arm already authored hanging (an A-pose FBX) keeps its own pose, so
//! a model authored at ease still stands exactly as authored.

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{Quat, Vec3};

/// How far below the horizontal an arm hangs at ease, in degrees: the kit's A-pose (58.6° at default proportions,
/// ADR 0014), rounded.
pub const HANG_DEG: f32 = 60.0;
/// An arm whose bind hangs less than this many degrees below the horizontal is a T-pose arm, lowered at ease.
pub const T_ARM_DEG: f32 = 20.0;
/// How far the elbow bends forward at ease, in degrees.
pub const ELBOW_DEG: f32 = 10.0;

/// `bind` with its T-pose arms lowered to hang at ease; every other bone, and an arm already hanging, as `bind` has it.
#[must_use]
pub fn at_ease(bind: &HumanoidPose) -> HumanoidPose {
    let mut pose = *bind;
    // `side` is +1 for the left arm, which lies along +X in the T-pose, and -1 for the right, along -X.
    for (side, shoulder, upper, lower) in [
        (1.0, HumanoidBone::LeftShoulder, HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm),
        (-1.0, HumanoidBone::RightShoulder, HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm),
    ] {
        let shoulder_turn = bind.get(shoulder);
        let along = (shoulder_turn * bind.get(upper)) * (Vec3::X * side);
        let hang = (-along.y).clamp(-1.0, 1.0).asin().to_degrees();
        if hang >= T_ARM_DEG {
            continue;
        }
        // The arm turns down in its own vertical plane, about the axis across it, so it ends exactly at the hang. The
        // turn is made in the upper arm's parent frame, the shoulder's, so the axis is carried into it.
        let across = along.cross(Vec3::NEG_Y).normalize();
        let lower_by = Quat::from_axis_angle(shoulder_turn.inverse() * across, (HANG_DEG - hang).to_radians());
        pose.set(upper, (lower_by * bind.get(upper)).normalize());
        // The forearm bends about the arm's own up axis, toward the front.
        let elbow = Quat::from_rotation_y(-side * ELBOW_DEG.to_radians());
        pose.set(lower, (elbow * bind.get(lower)).normalize());
    }
    pose
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How far below the horizontal the arm on `side` hangs in `pose`, and how far forward its hand reaches, from the
    /// chest: the shoulder and arm turns applied to the T-pose arm.
    fn arm(pose: &HumanoidPose, side: f32, bones: [HumanoidBone; 3]) -> (f32, f32) {
        let [shoulder, upper, lower] = bones;
        let upper_turn = pose.get(shoulder) * pose.get(upper);
        let dir = upper_turn * (Vec3::X * side);
        let hand = dir + upper_turn * pose.get(lower) * (Vec3::X * side);
        ((-dir.y).asin().to_degrees(), hand.z)
    }

    const LEFT: [HumanoidBone; 3] = [HumanoidBone::LeftShoulder, HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm];
    const RIGHT: [HumanoidBone; 3] =
        [HumanoidBone::RightShoulder, HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm];

    #[test]
    fn a_t_pose_lowers_both_arms_to_hang_and_bends_the_elbows_forward() {
        let pose = at_ease(&HumanoidPose::REST);
        for (side, bones) in [(1.0, LEFT), (-1.0, RIGHT)] {
            let (hang, forward) = arm(&pose, side, bones);
            assert!((hang - HANG_DEG).abs() < 1e-3, "the arm hangs {hang}°");
            assert!(forward > 0.1, "the hand comes forward: {forward}");
        }
        assert_eq!(pose.mirrored(), pose, "left and right mirror each other");
        assert_eq!(pose.get(HumanoidBone::Head), Quat::IDENTITY, "nothing else moves");
    }

    #[test]
    fn a_tilted_shoulder_still_ends_at_the_hang() {
        let mut bind = HumanoidPose::REST;
        bind.set(HumanoidBone::LeftShoulder, Quat::from_rotation_z(0.15) * Quat::from_rotation_y(0.2));
        let (hang, _) = arm(&at_ease(&bind), 1.0, LEFT);
        assert!((hang - HANG_DEG).abs() < 1e-3, "the arm hangs {hang}°");
    }

    #[test]
    fn an_arm_authored_hanging_keeps_its_pose() {
        let mut bind = HumanoidPose::REST;
        bind.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(-45_f32.to_radians()));
        bind.set(HumanoidBone::RightUpperArm, Quat::from_rotation_z(45_f32.to_radians()));
        assert_eq!(at_ease(&bind), bind);
    }
}
