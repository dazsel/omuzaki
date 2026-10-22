//! Hair physics presets (#321): named feels for a hair part's motion, which a person or an agent picks before tuning.
//!
//! A preset sets the part's [`HairPhysics`] (how much velocity a step keeps, gravity, wind, constraint passes) and the
//! stiffness of every simulated group (how firmly each point springs back to the styled shape, root to tip). It leaves
//! the simulation rate, the collider margin and the part's own colliders alone: those are the hair's fit, not its
//! feel. `Silky` is the default every new hair part starts with.
//!
//! Measured on the kit's hair with `hair.settle` (`tests/hair_physics.rs`): after a 25 degree turn over 0.2 s, silky
//! swings 39 mm off its turned rest and settles within 2 mm of it in 0.58 s, bouncy 21 mm and 0.32 s, heavy 10 mm and
//! 0.12 s, and stiff never leaves the 2 mm band (1.9 mm).

use omuzaki_char_api::{Falloff, HairPhysics};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A named feel for the hair's motion.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// The default: light, swings freely and springs back slowly.
    Silky,
    /// Long thick hair: falls hard, swings little, settles fast.
    Heavy,
    /// Short springy hair: a strong spring and little loss, a quick rebound.
    Bouncy,
    /// Almost rigid: sprayed or very short hair that barely moves.
    Stiff,
}

impl Preset {
    /// Every preset.
    pub const ALL: [Self; 4] = [Self::Silky, Self::Heavy, Self::Bouncy, Self::Stiff];

    /// The preset's name, as the ops take it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Silky => "silky",
            Self::Heavy => "heavy",
            Self::Bouncy => "bouncy",
            Self::Stiff => "stiff",
        }
    }

    /// `current` with this preset's feel: its damping, gravity, wind and passes, keeping `current`'s rate, collider
    /// margin and colliders.
    pub fn physics(self, current: &HairPhysics) -> HairPhysics {
        let base = HairPhysics {
            fixed_hz: current.fixed_hz,
            collider_margin: current.collider_margin,
            colliders: current.colliders.clone(),
            ..HairPhysics::default()
        };
        match self {
            Self::Silky => base,
            Self::Heavy => HairPhysics { damping: 0.9, gravity_scale: 0.8, wind_scale: 0.4, iterations: 3, ..base },
            Self::Bouncy => HairPhysics { damping: 0.985, gravity_scale: 0.25, wind_scale: 1.2, ..base },
            Self::Stiff => HairPhysics { damping: 0.88, gravity_scale: 0.2, wind_scale: 0.3, iterations: 3, ..base },
        }
    }

    /// The stiffness this preset gives every simulated group, root to tip.
    pub fn stiffness(self) -> Falloff {
        let base = Falloff::default();
        match self {
            Self::Silky => base,
            Self::Heavy => Falloff { tip: 0.15, ..base },
            Self::Bouncy => Falloff { tip: 0.12, from: 0.2, ..base },
            Self::Stiff => Falloff { tip: 0.4, from: 0.4, to: 1.0, ..base },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_face_api::Validate;

    #[test]
    fn silky_is_the_default_and_every_preset_is_valid() {
        let current = HairPhysics::default();
        assert_eq!(Preset::Silky.physics(&current), current);
        assert_eq!(Preset::Silky.stiffness(), Falloff::default());
        for preset in Preset::ALL {
            assert!(preset.physics(&current).validate().is_ok(), "{preset:?}");
            assert!(preset.stiffness().validate().is_ok(), "{preset:?}");
            let named: Preset = serde_json::from_value(serde_json::json!(preset.name())).unwrap_or(Preset::Silky);
            assert_eq!(named, preset);
        }
    }

    #[test]
    fn a_preset_keeps_the_hairs_fit() {
        let current = HairPhysics { fixed_hz: 120.0, collider_margin: 0.012, ..HairPhysics::default() };
        let heavy = Preset::Heavy.physics(&current);
        assert_eq!((heavy.fixed_hz, heavy.collider_margin), (120.0, 0.012));
    }
}
