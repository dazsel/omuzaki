//! The gaze route and the look-at range maps (design D3, D4; VRM 1.0 `lookAt.md`).
//!
//! The eight `eyeLook*` channels reach an imported model through exactly one route, chosen once at prepare time and
//! stored in the avatar ([`AvatarGaze`]), so a symmetric gaze can never reach the same preset twice. The range maps
//! turn a gaze angle into an eye-bone rotation (bone route) or a look-preset weight (presets route).
//!
//! **Gaze in degrees.** A channel value of 1 is a full-weight look, [`LOOK_FULL_RAD`] (40°); range maps read degrees,
//! as VRM writes them.

use omuzaki_anim_api::Channel;
use omuzaki_anim_api::retarget::LOOK_FULL_RAD;
use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{LookAt, LookAtKind, PresetExpression, RangeMap};

/// How the eight `eyeLook*` channels reach the model: one route, chosen at prepare time, first match wins.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AvatarGaze {
    /// The look-at stage turns the channels into eye-bone rotations; no gaze expression is driven. Chosen when the
    /// file's look-at is bone type (or absent) and both eye bones exist.
    Bone,
    /// Each `eyeLook*` channel drives the model's own expression of that name, like any `ARKit` channel; look-at drives
    /// nothing and the look presets are not driven.
    NativeArkit,
    /// The channels become the four look-preset weights through the range maps (VRM's expression type).
    Presets,
    /// Nothing can show gaze on this model (reported as `arkit.gaze_inert`).
    Inert,
}

/// The four directions of look-at, as range maps and presets name them.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LookDirection {
    /// Up.
    Up,
    /// Down.
    Down,
    /// Toward the character's left.
    Left,
    /// Toward the character's right.
    Right,
}

impl LookDirection {
    /// All four, in preset order.
    pub const ALL: [Self; 4] = [Self::Up, Self::Down, Self::Left, Self::Right];

    /// The look preset of this direction.
    pub const fn preset(self) -> PresetExpression {
        match self {
            Self::Up => PresetExpression::LookUp,
            Self::Down => PresetExpression::LookDown,
            Self::Left => PresetExpression::LookLeft,
            Self::Right => PresetExpression::LookRight,
        }
    }

    /// The two `eyeLook*` channels this direction's preset shows: looking toward the character's left turns the left
    /// eye out and the right eye in.
    pub const fn channels(self) -> [Channel; 2] {
        match self {
            Self::Up => [Channel::EYE_LOOK_UP_LEFT, Channel::EYE_LOOK_UP_RIGHT],
            Self::Down => [Channel::EYE_LOOK_DOWN_LEFT, Channel::EYE_LOOK_DOWN_RIGHT],
            Self::Left => [Channel::EYE_LOOK_OUT_LEFT, Channel::EYE_LOOK_IN_RIGHT],
            Self::Right => [Channel::EYE_LOOK_IN_LEFT, Channel::EYE_LOOK_OUT_RIGHT],
        }
    }
}

/// The look-at settings of an imported avatar: the fields of the file's `lookAt`.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AvatarLookAt {
    /// Bone or expression, as the file says.
    pub kind: LookAtKind,
    /// The gaze origin relative to the head bone, in the head's rest frame.
    pub offset_from_head_bone: Vec3,
    /// Inward horizontal range: each eye turning toward the nose (bone route).
    pub horizontal_inner: RangeMap,
    /// Outward horizontal range: each eye turning away from the nose (bone route), and both left and right on the
    /// presets route (VRM's expression type reads only this horizontal map).
    pub horizontal_outer: RangeMap,
    /// Downward range.
    pub vertical_down: RangeMap,
    /// Upward range.
    pub vertical_up: RangeMap,
}

/// The default range map's input: 90° (VRM 1.0 `VRMC_vrm.lookAt.rangeMap.schema.json`).
pub const DEFAULT_INPUT_MAX: f32 = 90.0;
/// The default output of a bone-type range map: 10° of eye rotation (the same schema).
pub const DEFAULT_BONE_OUTPUT: f32 = 10.0;
/// The default output of an expression-type range map: full preset weight, as `three-vrm` and `UniVRM` use.
pub const DEFAULT_EXPRESSION_OUTPUT: f32 = 1.0;

impl AvatarLookAt {
    /// Look-at of `kind` with every range map at the default for that kind and no offset: what a file without a
    /// `lookAt` means.
    pub const fn default_for(kind: LookAtKind) -> Self {
        let output = match kind {
            LookAtKind::Bone => DEFAULT_BONE_OUTPUT,
            LookAtKind::Expression => DEFAULT_EXPRESSION_OUTPUT,
        };
        let map = RangeMap { input_max_value: DEFAULT_INPUT_MAX, output_scale: output };
        Self {
            kind,
            offset_from_head_bone: Vec3::ZERO,
            horizontal_inner: map,
            horizontal_outer: map,
            vertical_down: map,
            vertical_up: map,
        }
    }

    /// The range map the presets route reads for `direction`.
    pub const fn preset_map(&self, direction: LookDirection) -> RangeMap {
        match direction {
            LookDirection::Up => self.vertical_up,
            LookDirection::Down => self.vertical_down,
            LookDirection::Left | LookDirection::Right => self.horizontal_outer,
        }
    }

    /// The bone route's directions: each range map with the two channels it turns into eye rotation.
    pub const fn bone_maps(&self) -> [(RangeMap, [Channel; 2]); 4] {
        [
            (self.vertical_up, [Channel::EYE_LOOK_UP_LEFT, Channel::EYE_LOOK_UP_RIGHT]),
            (self.vertical_down, [Channel::EYE_LOOK_DOWN_LEFT, Channel::EYE_LOOK_DOWN_RIGHT]),
            (self.horizontal_inner, [Channel::EYE_LOOK_IN_LEFT, Channel::EYE_LOOK_IN_RIGHT]),
            (self.horizontal_outer, [Channel::EYE_LOOK_OUT_LEFT, Channel::EYE_LOOK_OUT_RIGHT]),
        ]
    }
}

impl From<LookAt> for AvatarLookAt {
    fn from(l: LookAt) -> Self {
        Self {
            kind: l.kind,
            offset_from_head_bone: l.offset_from_head_bone,
            horizontal_inner: l.horizontal_inner,
            horizontal_outer: l.horizontal_outer,
            vertical_down: l.vertical_down,
            vertical_up: l.vertical_up,
        }
    }
}

/// The output of `map` for a gaze of `degrees` (VRM 1.0 `lookAt.md`, "`RangeMap`"): linear from 0 at 0° to
/// `output_scale` at `input_max_value`, held there beyond it, 0 for a negative angle. A map with no input range
/// (`input_max_value` 0 or less) jumps straight to its output for any positive angle.
pub fn map_range(map: RangeMap, degrees: f32) -> f32 {
    if degrees.is_nan() || degrees <= 0.0 {
        return 0.0;
    }
    if map.input_max_value <= 0.0 {
        return map.output_scale;
    }
    (degrees / map.input_max_value).min(1.0) * map.output_scale
}

/// A channel value (look units, 1 = a full look) as degrees of gaze.
pub fn look_degrees(value: f32) -> f32 {
    value * LOOK_FULL_RAD.to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_map_is_linear_then_holds() {
        let m = RangeMap { input_max_value: 40.0, output_scale: 1.0 };
        assert_eq!(map_range(m, 20.0), 0.5);
        assert_eq!(map_range(m, 80.0), 1.0);
        assert_eq!(map_range(m, -5.0), 0.0);
        assert_eq!(map_range(m, f32::NAN), 0.0);
        assert_eq!(map_range(RangeMap { input_max_value: 0.0, output_scale: 0.7 }, 1.0), 0.7);
    }

    #[test]
    fn a_full_look_is_forty_degrees() {
        assert!((look_degrees(1.0) - 40.0).abs() < 1e-4);
    }

    #[test]
    fn defaults_follow_the_kind() {
        assert_eq!(AvatarLookAt::default_for(LookAtKind::Bone).vertical_up.output_scale, 10.0);
        assert_eq!(AvatarLookAt::default_for(LookAtKind::Expression).horizontal_outer.output_scale, 1.0);
    }

    #[test]
    fn left_turns_the_left_eye_out_and_the_right_eye_in() {
        assert_eq!(LookDirection::Left.channels(), [Channel::EYE_LOOK_OUT_LEFT, Channel::EYE_LOOK_IN_RIGHT]);
        assert_eq!(LookDirection::Right.channels(), [Channel::EYE_LOOK_IN_LEFT, Channel::EYE_LOOK_OUT_RIGHT]);
    }
}
