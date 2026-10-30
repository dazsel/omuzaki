//! The living body's dials (spec PF-24, AP-05): how much a character's body moves on its own under a tracked or voiced
//! face. The [`Animator`](crate::Animator) that reads them (`omuzaki_anim::body::LivingBody`) infers the torso, the
//! shoulders, the arms and co-speech gestures from the bus and the voice cues, so a webcam-only performer or a voiced
//! AI character has a body that breathes, shifts its weight, follows the head and talks with its hands.
//!
//! A [`BodyStyle`] is plain `Copy` data: it crosses to the perform thread in a queue slot and is applied between two
//! steps, allocating nothing. Every amount is a multiplier on the preset's tuned motion, `1` as tuned and `0` off, so
//! a style of all zeros is the bus animator alone, bit for bit.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::life::Personality;

/// Largest value of any [`BodyStyle`] amount.
pub const MAX_BODY_AMOUNT: f32 = 2.0;

/// Which hand leads one-handed gestures.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Handedness {
    /// The right hand leads (about two gestures in three); the left takes the rest.
    #[default]
    Right,
    /// The left hand leads.
    Left,
}

/// How a character's body moves on its own. Every amount is `0..=2`: `0` turns that part off, `1` is the preset's
/// tuned motion and `2` doubles it. Out-of-range and non-finite amounts are held to the range by [`BodyStyle::clamped`].
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BodyStyle {
    /// The temperament the motion is tuned for: how big, how fast and how often.
    pub preset: Personality,
    /// Master amount over everything below. `0` leaves only the tracked head, the lean and the breath.
    pub amount: f32,
    /// Weight shifts and the slow sway of the torso.
    pub sway: f32,
    /// How much the shoulders and chest turn with the head (the head itself stays exactly where tracking puts it).
    pub follow: f32,
    /// Breath in the chest and shoulders.
    pub breath: f32,
    /// Posture from the face: proud on a smile, slumped when sad, shoulders up in anger or surprise, shaking in a laugh.
    pub emotion: f32,
    /// Co-speech gestures: how often and how big. `0` keeps the hands still while talking.
    pub gestures: f32,
    /// The arms' follow-through when the torso moves.
    pub arms: f32,
    /// Small idle adjustments between phrases (a shoulder roll, an arm resettled, a hand opening).
    pub fidgets: f32,
    /// The leading hand.
    pub handedness: Handedness,
}

impl Default for BodyStyle {
    fn default() -> Self {
        Self::preset(Personality::Natural)
    }
}

impl BodyStyle {
    /// Every amount at `1` for `preset`.
    pub const fn preset(preset: Personality) -> Self {
        Self {
            preset,
            amount: 1.0,
            sway: 1.0,
            follow: 1.0,
            breath: 1.0,
            emotion: 1.0,
            gestures: 1.0,
            arms: 1.0,
            fidgets: 1.0,
            handedness: Handedness::Right,
        }
    }

    /// No body motion of its own: the bus animator's pose exactly.
    pub const OFF: Self = Self {
        preset: Personality::Natural,
        amount: 0.0,
        sway: 0.0,
        follow: 0.0,
        breath: 0.0,
        emotion: 0.0,
        gestures: 0.0,
        arms: 0.0,
        fidgets: 0.0,
        handedness: Handedness::Right,
    };

    /// The amounts in order, with their names: what an op checks and reports.
    pub fn amounts(&self) -> [(&'static str, f32); 8] {
        [
            ("amount", self.amount),
            ("sway", self.sway),
            ("follow", self.follow),
            ("breath", self.breath),
            ("emotion", self.emotion),
            ("gestures", self.gestures),
            ("arms", self.arms),
            ("fidgets", self.fidgets),
        ]
    }

    /// The same style with every amount held to `0..=MAX_BODY_AMOUNT`; NaN reads as `0`.
    #[must_use]
    pub fn clamped(self) -> Self {
        let c = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, MAX_BODY_AMOUNT) };
        Self {
            amount: c(self.amount),
            sway: c(self.sway),
            follow: c(self.follow),
            breath: c(self.breath),
            emotion: c(self.emotion),
            gestures: c(self.gestures),
            arms: c(self.arms),
            fidgets: c(self.fidgets),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamping_holds_every_amount_and_reads_nan_as_zero() {
        let wild = BodyStyle { amount: f32::NAN, sway: -1.0, gestures: 9.0, ..BodyStyle::default() }.clamped();
        assert_eq!(wild.amount, 0.0);
        assert_eq!(wild.sway, 0.0);
        assert_eq!(wild.gestures, MAX_BODY_AMOUNT);
        assert!(wild.amounts().iter().all(|(_, v)| (0.0..=MAX_BODY_AMOUNT).contains(v)));
    }

    #[test]
    fn a_style_round_trips_through_json_and_fills_defaults() {
        let style: BodyStyle = serde_json::from_str(r#"{"preset":"energetic","gestures":1.5}"#).expect("reads");
        assert_eq!(style.preset, Personality::Energetic);
        assert_eq!(style.gestures, 1.5);
        assert_eq!(style.sway, 1.0);
        let back: BodyStyle = serde_json::from_value(serde_json::to_value(style).expect("writes")).expect("reads");
        assert_eq!(back, style);
    }
}
