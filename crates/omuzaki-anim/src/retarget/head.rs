//! The head channels: `headYaw`, `headPitch`, `headRoll` (measured from the calibrated rest pose), `headShiftX/Y`
//! and `lean` (movement toward the camera).
//!
//! The prototype let these run past any limit (a head gain of 1.4 turned a 3 rad yaw into 4.2). The contract promises
//! every channel stays inside its declared range, so the output is clamped to it; the filters still see the unclamped
//! signal, so a head held past the limit does not change how the motion is smoothed.

use omuzaki_anim_api::{Channel, ChannelBuf, ChannelMask, HeadPose, OneEuroParams};
use omuzaki_core::glam::Vec3;

use super::filter::OneEuro;

/// Radians to the degrees the prototype smoothed head angles in.
const DEGREES: f32 = 180.0 / std::f32::consts::PI;
/// Metres to the centimetres the prototype smoothed head shift in.
const CENTIMETRES: f32 = 100.0;

/// The head channels in state order, each with the factor from its unit (radians, metres) to the unit the prototype's
/// filters ran in. The One-Euro speed term (`beta * |dx|`) depends on the signal's units, so the filters keep running
/// in the prototype's (degrees for angles and lean, centimetres for shift) and the result is converted back; filtering
/// in radians would open the cutoff 57 times less for the same motion and change how the head feels.
const AXES: [(Channel, f32); 6] = [
    (Channel::HEAD_YAW, DEGREES),
    (Channel::HEAD_PITCH, DEGREES),
    (Channel::HEAD_ROLL, DEGREES),
    (Channel::HEAD_SHIFT_X, CENTIMETRES),
    (Channel::HEAD_SHIFT_Y, CENTIMETRES),
    (Channel::LEAN, DEGREES),
];

/// The angles every head pose has.
const ANGLES: ChannelMask = ChannelMask::from_channels(&[Channel::HEAD_YAW, Channel::HEAD_PITCH, Channel::HEAD_ROLL]);
/// The channels that need a position, from both the frame and the rest pose.
const OFFSETS: ChannelMask = ChannelMask::from_channels(&[Channel::HEAD_SHIFT_X, Channel::HEAD_SHIFT_Y, Channel::LEAN]);

/// The retarget settings the head cares about.
#[derive(Copy, Clone, Debug)]
pub(super) struct HeadTuning {
    /// Multiplier on the angles.
    pub gain: f32,
    /// Lean in radians per metre toward the camera.
    pub lean_per_metre: f32,
    /// `None` for no smoothing.
    pub filter: Option<OneEuroParams>,
}

/// The head filters.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct HeadChannels {
    filters: [OneEuro; AXES.len()],
}

impl HeadChannels {
    /// Writes the head channels for `head`, measured from `rest`, into `values` (unmirrored) and returns the channels
    /// written. The angles are always there; shift and lean only when both poses have a position, since without a
    /// rest position there is no zero to measure from.
    ///
    /// Every value is held to its channel's legal range (see the module docs).
    ///
    /// A pose with a NaN or infinity in it is a glitch, not a pose: the channels are still driven, but toward rest,
    /// which eases the head back through the filters instead of jumping (the prototype did the same).
    pub fn apply(
        &mut self,
        head: HeadPose,
        rest: HeadPose,
        tuning: HeadTuning,
        dt: f32,
        values: &mut ChannelBuf,
    ) -> ChannelMask {
        let sound = head.yaw.is_finite()
            && head.pitch.is_finite()
            && head.roll.is_finite()
            && head.position.is_none_or(Vec3::is_finite);
        let mut mask = ANGLES;
        let mut target = [0.0; AXES.len()];
        if sound {
            target[..3].copy_from_slice(&[
                (head.yaw - rest.yaw) * tuning.gain,
                (head.pitch - rest.pitch) * tuning.gain,
                (head.roll - rest.roll) * tuning.gain,
            ]);
        }
        if let Some((now, at_rest)) = head.position.zip(rest.position) {
            mask |= OFFSETS;
            if sound {
                let moved = now - at_rest;
                target[3..].copy_from_slice(&[moved.x, moved.y, moved.z * tuning.lean_per_metre]);
            }
        }
        for ((channel, unit), (state, target)) in AXES.iter().zip(self.filters.iter_mut().zip(target)) {
            if mask.contains(*channel) {
                let (min, max) = channel.range();
                values[*channel] = (state.filter(tuning.filter, target * unit, dt) / unit).clamp(min, max);
            }
        }
        mask
    }

    /// Forgets the history of every head filter.
    pub fn reset(&mut self) {
        self.filters = Default::default();
    }
}
