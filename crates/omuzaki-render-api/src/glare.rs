//! Lens glare: what a camera's lens does with a light too bright for it. Bloom spreads a bright area into a round glow;
//! glare adds the lens's own marks: a star of streaks through the light (from the blades of the iris, or a filter),
//! a soft ring (halo) around it and faint ghosts of the light reflected across the frame, the way an animated-feature frame shows a
//! spell in a hand or a lamp in shot.
//!
//! Only what is brighter than `threshold` (linear luminance before exposure, like bloom's) flares, so a frame with no
//! light that bright is drawn exactly as without glare. Lengths are fractions of the output's height, so a look reads
//! the same at any resolution.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Colour, RenderError};

/// The most streaks a glare draws through each light.
pub const MAX_STREAKS: u32 = 8;

/// Lens glare from the brightest lights: streaks, a halo and ghosts.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Glare {
    /// How bright the glare is against the light that makes it, `0..=4`.
    #[schemars(range(min = 0.0, max = 4.0))]
    pub strength: f32,
    /// Linear luminance above which a light flares, before exposure, `>= 0`.
    #[schemars(range(min = 0.0))]
    pub threshold: f32,
    /// How many streaks pass through each light: 0 (none), 2 (one line, an anamorphic flare), 4, 6 or 8 (a star).
    #[schemars(range(min = 0, max = 8))]
    pub streaks: u32,
    /// How far a streak reaches from its light, as a fraction of the output height, `0.01..=1`.
    #[schemars(range(min = 0.01, max = 1.0))]
    pub length: f32,
    /// The first streak's angle, degrees anticlockwise from horizontal.
    pub rotation_deg: f32,
    /// The colour the glare is multiplied by: white keeps the light's own colour.
    pub tint: Colour,
    /// A soft ring around each light, a twelfth of the frame's height across from it, `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub halo: f32,
    /// Faint, out-of-focus reflections of each light along the line through the frame's centre, `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub ghosts: f32,
}

impl Default for Glare {
    fn default() -> Self {
        Self {
            strength: 1.0,
            threshold: 1.5,
            streaks: 4,
            length: 0.3,
            rotation_deg: 20.0,
            tint: Colour::WHITE,
            halo: 0.0,
            ghosts: 0.0,
        }
    }
}

impl Glare {
    /// True when nothing would be drawn.
    pub fn is_empty(&self) -> bool {
        self.strength <= 0.0 || (self.streaks == 0 && self.halo <= 0.0 && self.ghosts <= 0.0)
    }

    /// Checks every field.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] naming the field out of range.
    pub fn validate(&self) -> Result<(), RenderError> {
        let check = |ok: bool, field: &str, range: &str| {
            if ok {
                Ok(())
            } else {
                Err(RenderError::invalid("post", format!("glare.{field} must be a finite number {range}")))
            }
        };
        let within = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        check(within(self.strength, 0.0, 4.0), "strength", "in 0..=4")?;
        check(self.threshold.is_finite() && self.threshold >= 0.0, "threshold", ">= 0")?;
        if !self.streaks.is_multiple_of(2) || self.streaks > MAX_STREAKS {
            return Err(RenderError::invalid("post", "glare.streaks must be 0, 2, 4, 6 or 8"));
        }
        check(within(self.length, 0.01, 1.0), "length", "in 0.01..=1")?;
        check(self.rotation_deg.is_finite(), "rotation_deg", "of degrees")?;
        check(within(self.halo, 0.0, 1.0), "halo", "in 0..=1")?;
        check(within(self.ghosts, 0.0, 1.0), "ghosts", "in 0..=1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_valid_and_each_field_is_checked() {
        assert_eq!(Glare::default().validate(), Ok(()));
        assert!(!Glare::default().is_empty());
        for (bad, field) in [
            (Glare { strength: 5.0, ..Glare::default() }, "strength"),
            (Glare { threshold: -1.0, ..Glare::default() }, "threshold"),
            (Glare { streaks: 3, ..Glare::default() }, "streaks"),
            (Glare { streaks: 10, ..Glare::default() }, "streaks"),
            (Glare { length: 0.0, ..Glare::default() }, "length"),
            (Glare { rotation_deg: f32::NAN, ..Glare::default() }, "rotation_deg"),
            (Glare { halo: 2.0, ..Glare::default() }, "halo"),
            (Glare { ghosts: -0.5, ..Glare::default() }, "ghosts"),
        ] {
            assert!(bad.validate().unwrap_err().to_string().contains(field), "{field}");
        }
        assert!(Glare { streaks: 0, ..Glare::default() }.is_empty(), "no streaks, halo or ghosts");
        assert!(Glare { strength: 0.0, ..Glare::default() }.is_empty());
    }

    #[test]
    fn unknown_fields_are_refused_and_missing_ones_default() {
        let g: Glare = serde_json::from_value(serde_json::json!({"streaks": 6})).unwrap();
        assert_eq!(g, Glare { streaks: 6, ..Glare::default() });
        assert!(serde_json::from_value::<Glare>(serde_json::json!({"rays": 6})).is_err());
    }
}
