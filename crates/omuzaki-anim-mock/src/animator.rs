//! A stand-in animator: bus head, lean, breath and body-turn channels, layered onto the base pose it is given, with the prototype's
//! 35% neck and 65% head split. No clips (the pose library arrives with RG-06), so cues are ignored.

use omuzaki_anim_api::{
    Animator, BoneMask, Bus, Channel, Cue, HumanoidBone, HumanoidPose, PoseLayer, PoseLayers, PoseMode, Tick,
};
use omuzaki_core::glam::{EulerRot, Quat};

/// Share of the head angles the neck takes; the head takes the rest.
const NECK_SHARE: f32 = 0.35;
/// Radians the chest tips back per unit of breath.
const BREATH_TIP_RAD: f32 = 0.008;

/// See the module docs.
#[derive(Copy, Clone, Debug, Default)]
pub struct MockAnimator;

fn euler(yaw: f32, pitch: f32, roll: f32) -> Quat {
    Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll)
}

impl Animator for MockAnimator {
    fn animate(&mut self, _tick: Tick, bus: &Bus, _cues: &[Cue], out: &mut HumanoidPose) {
        let v = &bus.values;
        let (yaw, pitch, roll) = (v[Channel::HEAD_YAW], v[Channel::HEAD_PITCH], v[Channel::HEAD_ROLL]);
        let mut delta = HumanoidPose::REST;
        delta.set(HumanoidBone::Neck, euler(yaw * NECK_SHARE, pitch * NECK_SHARE, roll * NECK_SHARE));
        let head = 1.0 - NECK_SHARE;
        delta.set(HumanoidBone::Head, euler(yaw * head, pitch * head, roll * head));
        delta.set(HumanoidBone::Spine, Quat::from_rotation_x(v[Channel::LEAN]));
        delta.set(HumanoidBone::Chest, Quat::from_rotation_x(-BREATH_TIP_RAD * (v[Channel::BREATH] - 0.5)));
        delta.set(HumanoidBone::Hips, Quat::from_rotation_y(v[Channel::BODY_TURN]));
        // The bus drives these five bones as a delta on the base pose; everything else stays as given.
        let driven = BoneMask::of(&[
            HumanoidBone::Neck,
            HumanoidBone::Head,
            HumanoidBone::Spine,
            HumanoidBone::Chest,
            HumanoidBone::Hips,
        ]);
        let layer = PoseLayer { pose: delta, mask: driven, weight: 1.0, mode: PoseMode::Add };
        PoseLayers(&[layer]).resolve(out);
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // exact values copied through; small loop counters
mod tests {
    use super::*;

    #[test]
    fn head_angles_split_between_neck_and_head_and_the_sum_matches() {
        let mut bus = Bus::default();
        bus.values[Channel::HEAD_YAW] = 0.4;
        let mut pose = HumanoidPose::REST;
        MockAnimator.animate(Tick::at_rate(0, 30.0), &bus, &[], &mut pose);
        let (neck, head) = (pose.get(HumanoidBone::Neck), pose.get(HumanoidBone::Head));
        assert!((neck.angle_between(Quat::IDENTITY) - 0.14).abs() < 1e-5);
        assert!(((neck * head).angle_between(Quat::IDENTITY) - 0.4).abs() < 1e-5);
    }

    #[test]
    fn it_layers_onto_the_base_pose_and_leaves_other_bones_alone() {
        let mut base = HumanoidPose::REST;
        base.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(-1.0)); // an A bind
        base.set(HumanoidBone::Spine, Quat::from_rotation_x(0.1));
        let mut bus = Bus::default();
        bus.values[Channel::LEAN] = 0.2;
        let mut pose = base;
        MockAnimator.animate(Tick::at_rate(0, 30.0), &bus, &[], &mut pose);
        assert_eq!(pose.get(HumanoidBone::LeftUpperArm), base.get(HumanoidBone::LeftUpperArm));
        let spine = pose.get(HumanoidBone::Spine);
        assert!(spine.angle_between(Quat::from_rotation_x(0.3)) < 1e-5, "lean adds to the base spine");
    }

    #[test]
    fn a_neutral_bus_is_close_to_the_rest_pose() {
        let mut pose = HumanoidPose::REST;
        MockAnimator.animate(Tick::at_rate(0, 30.0), &Bus::default(), &[], &mut pose);
        for bone in HumanoidBone::ALL {
            assert!(pose.get(bone).angle_between(Quat::IDENTITY) < 0.01, "{bone}");
        }
    }
}
