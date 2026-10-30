//! The eye's design: every number that shapes one 3D eye and the skin round it, in physical units (metres, iris radii)
//! with defaults that build a believable adult eye. Serialisable, so an op can store and edit it like any other part.

use omuzaki_core::colour::Colour;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One eye and the skin round it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EyeDesign {
    /// Seeds every random detail (fibres, crypts, vessels, lashes, freckles), so one design always builds one eye.
    pub seed: u32,
    /// The eyeball.
    pub ball: Ball,
    /// The iris.
    pub iris: Iris,
    /// The white of the eye.
    pub sclera: Sclera,
    /// The opening between the lids.
    pub aperture: Aperture,
    /// The lids and the skin round the eye.
    pub skin: Skin,
    /// The lashes.
    pub lashes: Lashes,
}

impl Default for EyeDesign {
    fn default() -> Self {
        Self {
            seed: 1,
            ball: Ball::default(),
            iris: Iris::default(),
            sclera: Sclera::default(),
            aperture: Aperture::default(),
            skin: Skin::default(),
            lashes: Lashes::default(),
        }
    }
}

/// The eyeball's size and the cornea.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Ball {
    /// The eyeball's radius, metres (an adult's is about 12 mm).
    pub radius: f32,
    /// The iris's radius, metres (about 5.9 mm).
    pub iris_radius: f32,
    /// How far the cornea's apex stands past the eyeball's sphere, in radii.
    pub cornea_rise: f32,
    /// The pupil's radius, in iris radii.
    pub pupil: f32,
}

impl Default for Ball {
    fn default() -> Self {
        Self { radius: 0.012, iris_radius: 0.0059, cornea_rise: 0.09, pupil: 0.27 }
    }
}

/// The iris: a pigment model and its anatomy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Iris {
    /// The outer (ciliary) zone's colour.
    pub outer: Colour,
    /// The inner (pupillary) zone's colour, inside the collarette.
    pub inner: Colour,
    /// The colour of the deep layer showing through crypts and furrows.
    pub deep: Colour,
    /// The fibres' light colour.
    pub fibre: Colour,
    /// The limbal ring's colour.
    pub limbal: Colour,
    /// How strongly the radial fibres show, `0..=1`.
    pub fibres: f32,
    /// The collarette's distance from the pupil to the rim, `0..=1`.
    pub collarette: f32,
    /// How many crypts open round the collarette.
    pub crypts: u32,
    /// How many dark pigment specks.
    pub specks: u32,
    /// How many contraction furrows ring the outer zone.
    pub furrows: u32,
    /// The limbal ring's strength, `0..=1`.
    pub limbal_ring: f32,
}

impl Default for Iris {
    fn default() -> Self {
        Self {
            outer: Colour::hex(0x0092_602c),
            inner: Colour::hex(0x0064_3c1c),
            deep: Colour::hex(0x0024_140a),
            fibre: Colour::hex(0x00c4_8a48),
            limbal: Colour::hex(0x001e_120a),
            fibres: 0.8,
            collarette: 0.4,
            crypts: 40,
            specks: 70,
            furrows: 4,
            limbal_ring: 0.85,
        }
    }
}

/// The white of the eye.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Sclera {
    /// Its colour near the iris.
    pub white: Colour,
    /// Its colour toward the corners.
    pub edge: Colour,
    /// How many blood vessels reach in from the corners.
    pub vessels: u32,
    /// How strongly the fine vessel network shows, `0..=1`.
    pub network: f32,
}

impl Default for Sclera {
    fn default() -> Self {
        Self { white: Colour::hex(0x00cc_c2b6), edge: Colour::hex(0x00c4_a89c), vessels: 60, network: 0.7 }
    }
}

/// The opening between the lids, seen from the front, in metres from the eyeball's centre (`+x` toward the inner
/// corner, `+y` up).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Aperture {
    /// The inner corner's `x`.
    pub inner_x: f32,
    /// The outer corner's `x` (negative).
    pub outer_x: f32,
    /// The inner and outer corners' heights.
    pub inner_y: f32,
    /// The outer corner's height.
    pub outer_y: f32,
    /// How high the upper lid rises above the line between the corners, and where along it (0 outer, 1 inner) it
    /// peaks.
    pub upper: f32,
    /// Where the upper lid peaks.
    pub upper_peak: f32,
    /// How far the lower lid drops below that line, and where it is lowest.
    pub lower: f32,
    /// Where the lower lid is lowest.
    pub lower_peak: f32,
}

impl Default for Aperture {
    fn default() -> Self {
        Self {
            inner_x: 0.0128,
            outer_x: -0.0135,
            inner_y: -0.0022,
            outer_y: -0.0004,
            upper: 0.0052,
            upper_peak: 0.56,
            lower: 0.0049,
            lower_peak: 0.45,
        }
    }
}

/// The lids and the skin round the eye.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Skin {
    /// The skin's tone.
    pub tone: Colour,
    /// How many freckles, per square centimetre.
    pub freckles: f32,
    /// The freckles' colour.
    pub freckle: Colour,
    /// The lid's thickness over the eye, metres.
    pub lid_thickness: f32,
    /// The upper lid's crease: its distance above the lid's edge and its depth, metres.
    pub crease: f32,
    /// How deep the crease folds, metres.
    pub crease_depth: f32,
}

impl Default for Skin {
    fn default() -> Self {
        Self {
            tone: Colour::hex(0x00a0_725e),
            freckles: 40.0,
            freckle: Colour::hex(0x0070_3a24),
            lid_thickness: 0.0016,
            crease: 0.0058,
            crease_depth: 0.0014,
        }
    }
}

/// The lashes: strands rooted along the lids' front edges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lashes {
    /// How many on the upper and the lower lid.
    pub upper: u32,
    /// How many on the lower lid.
    pub lower: u32,
    /// The longest upper lash and the longest lower lash, metres.
    pub upper_length: f32,
    /// The longest lower lash, metres.
    pub lower_length: f32,
    /// How far they curl, `0..=1`.
    pub curl: f32,
    /// How tightly they gather into clumps, `0..=1`.
    pub clumping: f32,
    /// A lash's thickness at its root, metres.
    pub thickness: f32,
    /// Their colour.
    pub colour: Colour,
}

impl Default for Lashes {
    fn default() -> Self {
        Self {
            upper: 300,
            lower: 60,
            upper_length: 0.0048,
            lower_length: 0.003,
            curl: 0.8,
            clumping: 0.55,
            thickness: 0.000_07,
            colour: Colour::hex(0x0030_2018),
        }
    }
}
