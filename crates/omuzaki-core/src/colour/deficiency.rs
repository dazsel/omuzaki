//! Colour-vision deficiency: what a colour looks like to a dichromat (Machado, Oliveira and Fernandes 2009).
//!
//! The contrast checker (`omuzaki_char::palette::contrast`) and the UI both need this, so it lives with [`Colour`] rather
//! than in either of them. There is one copy of the matrices, and every caller gets the same result.

use glam::Vec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Colour;

/// One of the three dichromacies Machado et al. (2009) model.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Deficiency {
    /// No long-wave (red) cones.
    Protanopia,
    /// No middle-wave (green) cones.
    Deuteranopia,
    /// No short-wave (blue) cones.
    Tritanopia,
}

impl Deficiency {
    /// All three, in the order reports list them.
    pub const ALL: [Self; 3] = [Self::Protanopia, Self::Deuteranopia, Self::Tritanopia];

    /// The name a report uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Protanopia => "protanopia",
            Self::Deuteranopia => "deuteranopia",
            Self::Tritanopia => "tritanopia",
        }
    }

    /// Machado, Oliveira and Fernandes (2009), severity 1.0, applied to linear RGB (their Table 2 values, to six places).
    const fn matrix(self) -> [[f64; 3]; 3] {
        match self {
            Self::Protanopia => [
                [0.152_286, 1.052_583, -0.204_868],
                [0.114_503, 0.786_281, 0.099_216],
                [-0.003_882, -0.048_116, 1.051_998],
            ],
            Self::Deuteranopia => [
                [0.367_322, 0.860_646, -0.227_968],
                [0.280_085, 0.672_501, 0.047_413],
                [-0.011_820, 0.042_940, 0.968_881],
            ],
            Self::Tritanopia => [
                [1.255_528, -0.076_749, -0.178_779],
                [-0.078_411, 0.930_809, 0.147_602],
                [0.004_733, 0.691_367, 0.303_900],
            ],
        }
    }
}

/// The colour a dichromat sees: Machado et al. (2009) at severity 1.0, on linear RGB, clamped to `0..=1` and encoded.
/// Rounds to 8 bits like every colour the palette stores, so white stays white (each matrix row sums to one).
#[allow(clippy::cast_possible_truncation)] // the matrix output is clamped to 0..=1 first; the Colour API takes f32
pub fn simulate(colour: Colour, deficiency: Deficiency) -> Colour {
    let [r, g, b] = colour.to_linear().to_array().map(f64::from);
    let m = deficiency.matrix();
    let out = m.map(|row| (row[0] * r + row[1] * g + row[2] * b).clamp(0.0, 1.0));
    Colour::from_linear(Vec3::from_array(out.map(|v| v as f32)), 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure red, green and blue under each deficiency, pinned from the matrices (red also matches the contrast
    /// checker's own test). A change here means the simulation changed, not just the code around it.
    #[test]
    fn three_known_colours_under_each_deficiency() {
        let cases = [
            (Colour::rgb(255, 0, 0), Deficiency::Protanopia, Colour::rgb(109, 95, 0)),
            (Colour::rgb(255, 0, 0), Deficiency::Deuteranopia, Colour::rgb(163, 144, 0)),
            (Colour::rgb(255, 0, 0), Deficiency::Tritanopia, Colour::rgb(255, 0, 15)),
            (Colour::rgb(0, 255, 0), Deficiency::Protanopia, Colour::rgb(255, 229, 0)),
            (Colour::rgb(0, 255, 0), Deficiency::Deuteranopia, Colour::rgb(239, 214, 58)),
            (Colour::rgb(0, 255, 0), Deficiency::Tritanopia, Colour::rgb(0, 247, 217)),
            (Colour::rgb(0, 0, 255), Deficiency::Protanopia, Colour::rgb(0, 89, 255)),
            (Colour::rgb(0, 0, 255), Deficiency::Deuteranopia, Colour::rgb(0, 61, 251)),
            (Colour::rgb(0, 0, 255), Deficiency::Tritanopia, Colour::rgb(0, 107, 150)),
        ];
        for (colour, deficiency, want) in cases {
            assert_eq!(simulate(colour, deficiency), want, "{colour} under {}", deficiency.as_str());
        }
    }

    #[test]
    fn white_and_grey_are_fixed_points() {
        let grey = Colour::rgb(128, 128, 128);
        for d in Deficiency::ALL {
            assert_eq!(simulate(Colour::WHITE, d), Colour::WHITE, "{}", d.as_str());
            assert_eq!(simulate(grey, d), grey, "{}", d.as_str());
        }
    }
}
