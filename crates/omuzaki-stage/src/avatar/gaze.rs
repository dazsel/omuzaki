//! The bone route of an imported avatar's gaze (`docs/design/byom.md` D4; VRM 1.0 `lookAt.md`): the eight `eyeLook*`
//! channels turn the eye bones through the file's range maps. The other routes need nothing here: the native ARKit and
//! presets routes are expressions, which the evaluator drives.
//!
//! Axes are the normalised humanoid space the pose is written in: the character faces +Z, its left is +X, so each eye
//! turns about +Y toward +X for a look to the character's left, and about +X (negative) for a look up.

use omuzaki_anim_api::{Channel, ChannelBuf, HumanoidBone, HumanoidPose};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::gaze::{AvatarGaze, AvatarLookAt, look_degrees, map_range};
use omuzaki_core::glam::Quat;

/// The eye-bone look-at of an avatar on the bone route.
#[derive(Copy, Clone, Debug)]
pub(crate) struct EyeGaze {
    look: AvatarLookAt,
}

/// One eye's rotation for a gaze, in degrees: `toward_left` turns it toward the character's left, `up` upward.
fn eye_rotation(toward_left_deg: f32, up_deg: f32) -> Quat {
    Quat::from_rotation_y(toward_left_deg.to_radians()) * Quat::from_rotation_x(-up_deg.to_radians())
}

impl EyeGaze {
    /// The look-at of `avatar`, or `None` when its gaze is not on the bone route.
    pub(crate) fn of(avatar: &ImportedAvatar) -> Option<Self> {
        (avatar.gaze == AvatarGaze::Bone).then(|| Self { look: avatar.look_at() })
    }

    /// Turns both eye bones of `pose` by the gaze `channels` carry, on top of what the pose already holds. Hot path:
    /// a few multiplications.
    pub(crate) fn apply(&self, channels: &ChannelBuf, pose: &mut HumanoidPose) {
        let deg = |channel: Channel| look_degrees(channels.get(channel));
        let l = &self.look;
        let vertical = |up: Channel, down: Channel| map_range(l.vertical_up, deg(up)) - map_range(l.vertical_down, deg(down));
        // The left eye's outer side is the character's left; the right eye's inner side is.
        let left_yaw = map_range(l.horizontal_outer, deg(Channel::EYE_LOOK_OUT_LEFT))
            - map_range(l.horizontal_inner, deg(Channel::EYE_LOOK_IN_LEFT));
        let right_yaw = map_range(l.horizontal_inner, deg(Channel::EYE_LOOK_IN_RIGHT))
            - map_range(l.horizontal_outer, deg(Channel::EYE_LOOK_OUT_RIGHT));
        let left = eye_rotation(left_yaw, vertical(Channel::EYE_LOOK_UP_LEFT, Channel::EYE_LOOK_DOWN_LEFT));
        let right = eye_rotation(right_yaw, vertical(Channel::EYE_LOOK_UP_RIGHT, Channel::EYE_LOOK_DOWN_RIGHT));
        pose.set(HumanoidBone::LeftEye, left * pose.get(HumanoidBone::LeftEye));
        pose.set(HumanoidBone::RightEye, right * pose.get(HumanoidBone::RightEye));
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec3;
    use omuzaki_io_api::LookAtKind;

    use super::*;

    fn gaze() -> EyeGaze {
        EyeGaze { look: AvatarLookAt::default_for(LookAtKind::Bone) }
    }

    fn forward(pose: &HumanoidPose, bone: HumanoidBone) -> Vec3 {
        pose.get(bone) * Vec3::Z
    }

    #[test]
    fn a_look_to_the_left_turns_both_eyes_toward_plus_x_by_the_range_map() {
        let mut channels = ChannelBuf::default();
        channels.set(Channel::EYE_LOOK_OUT_LEFT, 1.0);
        channels.set(Channel::EYE_LOOK_IN_RIGHT, 1.0);
        let mut pose = HumanoidPose::REST;
        gaze().apply(&channels, &mut pose);
        // A full look is 40 degrees of gaze; the default bone map turns 90 degrees of gaze into 10 of eye.
        let expected = (40.0f32 / 90.0 * 10.0).to_radians();
        for bone in [HumanoidBone::LeftEye, HumanoidBone::RightEye] {
            let f = forward(&pose, bone);
            assert!(f.x > 0.0 && f.y.abs() < 1e-6, "{bone:?} looks {f}");
            assert!((f.x.atan2(f.z) - expected).abs() < 1e-5);
        }
    }

    #[test]
    fn a_look_up_raises_both_eyes_and_no_gaze_leaves_them() {
        let mut channels = ChannelBuf::default();
        channels.set(Channel::EYE_LOOK_UP_LEFT, 0.5);
        channels.set(Channel::EYE_LOOK_UP_RIGHT, 0.5);
        let mut pose = HumanoidPose::REST;
        gaze().apply(&channels, &mut pose);
        assert!(forward(&pose, HumanoidBone::LeftEye).y > 0.0);
        assert!(forward(&pose, HumanoidBone::RightEye).y > 0.0);
        let mut rest = HumanoidPose::REST;
        gaze().apply(&ChannelBuf::default(), &mut rest);
        assert_eq!(rest, HumanoidPose::REST);
    }
}
