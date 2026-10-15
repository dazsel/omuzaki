//! The bus-driven [`Animator`]: head angles, lean, breath and body turn, layered onto the base pose.
//!
//! This is the real counterpart of `MockAnimator` (omuzaki-anim-mock), which stays as the test oracle. The maths is the
//! same: the neck takes [`BusAnimatorParams::neck_share`] of each head angle and the head takes the rest, lean tips the
//! spine, breath tips the chest, and body turn yaws the hips. Cues are ignored until the RG-06 clips exist.
//!
//! Benchmark: `cargo bench -p omuzaki-anim --bench animator`.

use omuzaki_anim_api::{
    Animator, BoneMask, Bus, Channel, Cue, HumanoidBone, HumanoidPose, PoseLayer, PoseLayers, PoseMode, Tick,
};
use omuzaki_core::glam::{EulerRot, Quat};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tuning of the bus-driven animator. Defaults are the prototype's.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BusAnimatorParams {
    /// Share of the head angles the neck takes; the head takes the rest. Default 0.35.
    pub neck_share: f32,
    /// Radians the chest tips back per unit of breath away from 0.5. Default 0.008.
    pub breath_tip_rad: f32,
}

impl Default for BusAnimatorParams {
    fn default() -> Self {
        Self { neck_share: 0.35, breath_tip_rad: 0.008 }
    }
}

/// Bus head angles, lean, breath and body turn, layered onto the base pose. Cues are ignored until RG-06 clips exist.
///
/// Stateless: [`Animator::reset`] does nothing. Each frame builds one delta pose on the stack and adds it onto the five
/// bones it drives, so `animate` allocates nothing. A non-finite bus value reads as 0, so the pose is never NaN.
#[derive(Clone, Debug)]
pub struct BusAnimator {
    params: BusAnimatorParams,
    driven: BoneMask,
}

impl Default for BusAnimator {
    /// The prototype's tuning; the same pose as `MockAnimator` for a finite bus.
    fn default() -> Self {
        Self::new(BusAnimatorParams::default())
    }
}

impl BusAnimator {
    /// An animator with `params`. A non-finite field takes its default, and `neck_share` is clamped to `0..=1`.
    pub fn new(params: BusAnimatorParams) -> Self {
        let defaults = BusAnimatorParams::default();
        let params = BusAnimatorParams {
            neck_share: if params.neck_share.is_finite() {
                params.neck_share.clamp(0.0, 1.0)
            } else {
                defaults.neck_share
            },
            breath_tip_rad: if params.breath_tip_rad.is_finite() {
                params.breath_tip_rad
            } else {
                defaults.breath_tip_rad
            },
        };
        let driven = BoneMask::of(&[
            HumanoidBone::Neck,
            HumanoidBone::Head,
            HumanoidBone::Spine,
            HumanoidBone::Chest,
            HumanoidBone::Hips,
        ]);
        Self { params, driven }
    }

    /// The tuning in effect, after clamping and defaults.
    pub fn params(&self) -> BusAnimatorParams {
        self.params
    }
}

/// A bus value, or 0 if it is NaN or infinite, so no quaternion can be built from it.
fn finite(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn euler(yaw: f32, pitch: f32, roll: f32) -> Quat {
    Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll)
}

impl Animator for BusAnimator {
    fn animate(&mut self, _tick: Tick, bus: &Bus, _cues: &[Cue], out: &mut HumanoidPose) {
        let v = &bus.values;
        let (yaw, pitch, roll) =
            (finite(v[Channel::HEAD_YAW]), finite(v[Channel::HEAD_PITCH]), finite(v[Channel::HEAD_ROLL]));
        let neck = self.params.neck_share;
        let head = 1.0 - neck;
        let mut delta = HumanoidPose::REST;
        delta.set(HumanoidBone::Neck, euler(yaw * neck, pitch * neck, roll * neck));
        delta.set(HumanoidBone::Head, euler(yaw * head, pitch * head, roll * head));
        delta.set(HumanoidBone::Spine, Quat::from_rotation_x(finite(v[Channel::LEAN])));
        let breath = finite(v[Channel::BREATH]);
        delta.set(HumanoidBone::Chest, Quat::from_rotation_x(-self.params.breath_tip_rad * (breath - 0.5)));
        delta.set(HumanoidBone::Hips, Quat::from_rotation_y(finite(v[Channel::BODY_TURN])));
        // One layer over the five driven bones; bones it does not drive keep the base pose exactly.
        let layer = PoseLayer { pose: delta, mask: self.driven, weight: 1.0, mode: PoseMode::Add };
        PoseLayers(std::slice::from_ref(&layer)).resolve(out);
    }

    fn reset(&mut self) {}
}
