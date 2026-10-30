//! Makeup (#2472) and the shape of the teeth and tongue (#2473). Like the eye's detail, every part is off by default (a
//! strength of zero, a scale of one), so a face that never sets them paints as before.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::check::{Check, Validate};
use crate::palette::ColourRef;

/// Eyeshadow: a wash of colour on the upper lid that fades toward the brow, and a touch under the outer lower lid. It
/// follows the lid as the eye closes, so it shows on a shut eye too.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EyeShadow {
    /// Opacity, 0 none to 1 full.
    pub strength: f64,
    /// Colour on the lid.
    pub colour: ColourRef,
    /// Colour it fades through toward the brow.
    pub crease_colour: ColourRef,
    /// How far it reaches above the lid at its tallest, in eye half-widths.
    pub height: f64,
    /// How far it sweeps past the outer corner, 0 not at all to 1 a long wing.
    pub wing: f64,
    /// Opacity of the touch under the outer lower lid, 0 none.
    pub lower: f64,
}

impl Default for EyeShadow {
    fn default() -> Self {
        Self {
            strength: 0.0,
            colour: ColourRef::rgb(0xd8, 0x86, 0x84),
            crease_colour: ColourRef::rgb(0xe8, 0xb0, 0xa4),
            height: 0.4,
            wing: 0.3,
            lower: 0.0,
        }
    }
}

impl Validate for EyeShadow {
    fn check(&self, c: &mut Check) {
        c.range("strength", self.strength, 0.0, 1.0);
        c.range("height", self.height, 0.05, 1.0);
        c.range("wing", self.wing, 0.0, 1.0);
        c.range("lower", self.lower, 0.0, 1.0);
    }
}

/// The upper teeth's shape (#2473): how tall the band is, the lines between teeth and the gum above them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TeethStyle {
    /// The band's height, as a multiple of the default.
    pub height: f64,
    /// Opacity of the fine lines between teeth, 0 a plain band (the anime default).
    pub gaps: f64,
    /// How many teeth show across the mouth.
    pub count: u32,
    /// Opacity of the gum band along the top of the teeth, 0 none.
    pub gum: f64,
    /// The gum's colour.
    pub gum_colour: ColourRef,
}

impl Default for TeethStyle {
    fn default() -> Self {
        Self { height: 1.0, gaps: 0.0, count: 8, gum: 0.0, gum_colour: ColourRef::rgb(0xe0, 0x8a, 0x92) }
    }
}

impl Validate for TeethStyle {
    fn check(&self, c: &mut Check) {
        c.range("height", self.height, 0.4, 1.8);
        c.range("gaps", self.gaps, 0.0, 1.0);
        c.rule(&["count"], (2..=16).contains(&self.count), "use a whole number from 2 to 16");
        c.range("gum", self.gum, 0.0, 1.0);
    }
}

/// The tongue's shape inside an open mouth (#2473).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TongueStyle {
    /// Its width, as a multiple of the default.
    pub width: f64,
    /// Its height, as a multiple of the default.
    pub height: f64,
    /// Opacity of the groove down its middle, 0 none.
    pub groove: f64,
}

impl Default for TongueStyle {
    fn default() -> Self {
        Self { width: 1.0, height: 1.0, groove: 0.0 }
    }
}

impl Validate for TongueStyle {
    fn check(&self, c: &mut Check) {
        c.range("width", self.width, 0.5, 1.4);
        c.range("height", self.height, 0.5, 1.6);
        c.range("groove", self.groove, 0.0, 1.0);
    }
}
