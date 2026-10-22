//! Hair physics presets: named starting points for `HairPhysics` that a person or an agent picks before tuning.

use omuzaki_char_api::HairPhysics;

/// A named feel for the hair's motion.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Preset {
    /// The default: light, settles in about a second.
    Silky,
    /// Long thick hair: falls hard, swings little.
    Heavy,
    /// Short springy hair: little damping, a quick rebound.
    Bouncy,
}

impl Preset {
    /// Every preset.
    pub const ALL: [Self; 3] = [Self::Silky, Self::Heavy, Self::Bouncy];

    /// The physics this preset sets; the colliders are left empty (the rig's are used).
    pub fn physics(self) -> HairPhysics {
        let base = HairPhysics::default();
        match self {
            Self::Silky => base,
            Self::Heavy => HairPhysics { damping: 0.94, gravity_scale: 0.6, wind_scale: 0.5, iterations: 3, ..base },
            Self::Bouncy => HairPhysics { damping: 0.985, gravity_scale: 0.25, wind_scale: 1.2, ..base },
        }
    }
}
