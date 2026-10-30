//! A physical reaction to play on a performing body (master checklist §6, "physical reactions"): a bonk on the head,
//! a shove on the shoulder, a fall. The [`Animator`](crate::Animator) that reads it (`omuzaki_anim::body::LivingBody`)
//! knocks an active ragdoll of the spine, head and arms with it and hands the body back to its animated (or tracked)
//! pose within [`Impact::recover_s`], exactly.
//!
//! An [`Impact`] is plain `Copy` data: it crosses to the perform thread in a queue slot and is applied between two
//! steps, allocating nothing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Largest [`Impact::strength`].
pub const MAX_IMPACT_STRENGTH: f32 = 3.0;
/// The shortest and longest recoveries, seconds.
pub const MIN_RECOVER_S: f32 = 0.3;
/// See [`MIN_RECOVER_S`].
pub const MAX_RECOVER_S: f32 = 10.0;

/// What happens to the body.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImpactKind {
    /// A blow: the struck part and everything below it are knocked and spring back; the muscles stay on.
    #[default]
    Hit,
    /// A fall: the blow knocks the body off balance, the muscles give way and the upper body slumps the way it was
    /// pushed until its joints stop it, then it pulls itself back up.
    Fall,
}

/// Where the blow lands.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyPart {
    /// The top of the head.
    #[default]
    Head,
    /// The face.
    Face,
    /// The middle of the chest.
    Chest,
    /// The stomach.
    Stomach,
    /// The character's left shoulder.
    LeftShoulder,
    /// The character's right shoulder.
    RightShoulder,
}

/// A blow on a performing body. Directions are in the character's space: `+X` is its left, `+Y` up, `+Z` the way it
/// faces.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Impact {
    /// A blow or a fall.
    pub kind: ImpactKind,
    /// Where it lands.
    pub at: BodyPart,
    /// The way the blow travels (it need not be unit length). `[0, 0, -1]`, the default, comes from in front and
    /// pushes the body back; `[0, -1, 0]` comes down on top.
    pub travel: [f32; 3],
    /// How hard, `0..=3`: `1` is a firm bonk that rocks the head about 15 degrees.
    pub strength: f32,
    /// Seconds until the body is back on its animated pose, exactly.
    pub recover_s: f32,
}

impl Default for Impact {
    fn default() -> Self {
        Self::hit(BodyPart::Head, [0.0, 0.0, -1.0], 1.0)
    }
}

impl Impact {
    /// A blow on `at` travelling along `travel`, recovering in 1.2 s.
    pub const fn hit(at: BodyPart, travel: [f32; 3], strength: f32) -> Self {
        Self { kind: ImpactKind::Hit, at, travel, strength, recover_s: 1.2 }
    }

    /// A fall started by a push on `at` along `travel`, recovering in 3 s.
    pub const fn fall(at: BodyPart, travel: [f32; 3], strength: f32) -> Self {
        Self { kind: ImpactKind::Fall, at, travel, strength, recover_s: 3.0 }
    }

    /// The same impact recovering in `seconds`.
    #[must_use]
    pub const fn recovering_in(self, seconds: f32) -> Self {
        Self { recover_s: seconds, ..self }
    }

    /// The impact held to the ranges: strength `0..=3`, recovery `0.3..=10` s, a travel of zero or not finite taken as
    /// the default (from in front). Non-finite numbers become their defaults.
    #[must_use]
    pub fn clamped(self) -> Self {
        let strength = if self.strength.is_finite() { self.strength.clamp(0.0, MAX_IMPACT_STRENGTH) } else { 1.0 };
        let recover_s =
            if self.recover_s.is_finite() { self.recover_s.clamp(MIN_RECOVER_S, MAX_RECOVER_S) } else { 1.2 };
        let [x, y, z] = self.travel;
        let len = (x * x + y * y + z * z).sqrt();
        let travel = if len.is_finite() && len > 1e-6 { [x / len, y / len, z / len] } else { [0.0, 0.0, -1.0] };
        Self { travel, strength, recover_s, ..self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamping_holds_every_number_to_its_range() {
        let wild = Impact { strength: f32::NAN, recover_s: 100.0, travel: [0.0, 0.0, 0.0], ..Impact::default() };
        let held = wild.clamped();
        assert!((held.strength - 1.0).abs() < f32::EPSILON);
        assert!((held.recover_s - MAX_RECOVER_S).abs() < f32::EPSILON);
        assert_eq!(held.travel, [0.0, 0.0, -1.0]);
        let long = Impact::hit(BodyPart::Chest, [3.0, 0.0, 4.0], 9.0).clamped();
        assert!((long.strength - MAX_IMPACT_STRENGTH).abs() < f32::EPSILON);
        assert!((long.travel[0] - 0.6).abs() < 1e-6 && (long.travel[2] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn an_impact_round_trips_through_json_with_defaults_for_what_is_left_out() {
        let parsed: Impact = serde_json::from_str(r#"{"kind":"fall","at":"left_shoulder"}"#).unwrap();
        assert_eq!(parsed.kind, ImpactKind::Fall);
        assert_eq!(parsed.at, BodyPart::LeftShoulder);
        assert!((parsed.strength - 1.0).abs() < f32::EPSILON);
        let back: Impact = serde_json::from_value(serde_json::to_value(parsed).unwrap()).unwrap();
        assert_eq!(back, parsed);
    }
}
