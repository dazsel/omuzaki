//! The 3D eye's design: every number that shapes one photoreal eye and the skin round it, in physical units (metres,
//! iris radii) with defaults that build a believable adult eye. It is stored on the face as `face/eye3d`
//! ([`FaceProfile::eye3d`](crate::FaceProfile::eye3d)), so `face.set_style` edits any one field by path and
//! `face.describe_style` lists them with their ranges; `omuzaki_face::eye3d` builds the meshes and textures from it.

use omuzaki_core::colour::Colour;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::check::{Check, Validate};

/// One eye and the skin round it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EyeDesign {
    /// Seeds every random detail (fibres, crypts, vessels, lashes, freckles), so one design always builds one eye.
    pub seed: u32,
    /// How the eye is drawn, from 0 (photoreal: refraction, fibres, vessels, pores and strand lashes) to 1 (anime: a
    /// flat painted iris with its highlights, a clean white, cel-shaded skin and an inked lash line). Between the two,
    /// every look that can blend does, so one eye morphs from one style to the other.
    pub style: f32,
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
    /// How the eye looks in the anime style: the painted iris and the inked lines.
    pub anime: Anime,
    /// Marks painted by hand on the skin, the white and the iris, over the detail the design generates, in the order
    /// painted (at most [`MAX_MARKS`]).
    pub paint: Vec<EyeMark>,
}

impl Default for EyeDesign {
    fn default() -> Self {
        Self {
            seed: 1,
            style: 0.0,
            ball: Ball::default(),
            iris: Iris::default(),
            sclera: Sclera::default(),
            aperture: Aperture::default(),
            skin: Skin::default(),
            lashes: Lashes::default(),
            anime: Anime::default(),
            paint: Vec::new(),
        }
    }
}

impl EyeDesign {
    /// An anime eye: the anime style, with the larger iris and the taller opening anime proportions use.
    pub fn anime() -> Self {
        let base = Self::default();
        Self {
            style: 1.0,
            ball: Ball { iris_radius: 0.0078, pupil: 0.3, cornea_rise: 0.04, ..base.ball },
            aperture: Aperture { upper: 0.006, upper_peak: 0.5, lower: 0.0042, lower_peak: 0.5, ..base.aperture },
            skin: Skin { tone: Colour::hex(0x00f6_dccf), freckles: 0.0, ..base.skin },
            ..base
        }
    }
}

/// The surface a mark is painted on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EyeSurface {
    /// The lids and the skin round the eye; points in metres in the eye's frame (`+x` toward the nose, `+y` up, the
    /// eyeball's centre at the origin).
    Skin,
    /// The white; points in metres across the eyeball as seen from straight in front, the eye looking straight ahead.
    Sclera,
    /// The iris; points in metres from the iris's centre in its own plane, the eye looking straight ahead.
    Iris,
}

/// One mark painted by hand on the eye: a dot (one point) or a stroke (two or more), placed exactly where it is wanted,
/// over the detail the design generates. It is how a freckle, a mole, a vessel, a crypt or a fleck is put in a given
/// place, and how an eye is redrawn mark by mark to match a picture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EyeMark {
    /// What it is painted on.
    pub on: EyeSurface,
    /// Where: one point for a dot, more for a stroke through them, in the surface's metres (see [`EyeSurface`]).
    pub points: Vec<[f32; 2]>,
    /// The dot's diameter or the stroke's width, metres (0.01 mm to 10 mm).
    pub size: f32,
    /// Its colour.
    pub colour: Colour,
    /// How opaque, 0 to 1.
    #[serde(default = "full")]
    pub opacity: f32,
    /// How soft a dot's edge is, 0 (crisp) to 1 (fading from its centre); strokes are drawn crisp.
    #[serde(default)]
    pub softness: f32,
    /// How much a stroke narrows toward its last point, 0 (not at all) to 1 (to nothing).
    #[serde(default)]
    pub taper: f32,
}

fn full() -> f32 {
    1.0
}

/// The most marks one eye holds.
pub const MAX_MARKS: usize = 4096;

impl Validate for EyeMark {
    fn check(&self, c: &mut Check) {
        c.rule(&["points"], (1..=512).contains(&self.points.len()), "a mark needs 1 to 512 points");
        let inside = self.points.iter().flatten().all(|v| v.is_finite() && v.abs() <= 0.05);
        c.rule(&["points"], inside, "every point must be within 5 cm of the eye");
        c.range_f32("size", self.size, 0.000_01, 0.01);
        c.range_f32("opacity", self.opacity, 0.0, 1.0);
        c.range_f32("softness", self.softness, 0.0, 1.0);
        c.range_f32("taper", self.taper, 0.0, 1.0);
    }
}

/// A named shape for the eye's opening, which sets the aperture (and, for some, the iris's size and the lids): the
/// quick way to an eye's shape before its numbers are tuned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EyeShape {
    /// The default adult eye: a soft almond, the inner corner a little lower.
    Natural,
    /// Wide open and round: the upper lid high over the iris, white showing above the lower lid, the outer corner
    /// tilted down; the look of an eye opened wide for a macro photograph.
    Wide,
    /// Long and narrow, the outer corner lifted.
    Almond,
    /// Round and tall, peaked in the middle.
    Round,
    /// A heavy upper lid that sits low over the iris, with a deeper fold above it.
    Hooded,
    /// Narrow, the outer corner turned down.
    Downturned,
}

impl EyeShape {
    /// Every shape, in the order a picker lists them.
    pub const ALL: [Self; 6] = [Self::Natural, Self::Wide, Self::Almond, Self::Round, Self::Hooded, Self::Downturned];

    /// Gives `design` this shape: its aperture, and for `wide` the iris's and pupil's sizes and for `hooded` the lid's
    /// thickness and fold. Every other field keeps its value.
    pub fn apply(self, design: &mut EyeDesign) {
        let natural = Aperture::default();
        design.aperture = match self {
            Self::Natural => natural,
            Self::Wide => Aperture {
                inner_x: 0.011_8,
                outer_x: -0.015_1,
                inner_y: -0.003_3,
                outer_y: -0.001_2,
                upper: 0.010_6,
                upper_peak: 0.59,
                lower: 0.002_3,
                lower_peak: 0.53,
            },
            Self::Almond => Aperture {
                inner_y: -0.001_2,
                outer_y: 0.001_6,
                upper: 0.004_4,
                upper_peak: 0.48,
                lower: 0.003_2,
                lower_peak: 0.5,
                ..natural
            },
            Self::Round => Aperture { upper: 0.007_6, upper_peak: 0.5, lower: 0.005_4, lower_peak: 0.5, ..natural },
            Self::Hooded => Aperture { upper: 0.003_8, upper_peak: 0.6, lower: 0.004_2, ..natural },
            Self::Downturned => Aperture {
                inner_y: -0.000_6,
                outer_y: -0.003_2,
                upper: 0.004_6,
                upper_peak: 0.62,
                lower: 0.003_6,
                lower_peak: 0.4,
                ..natural
            },
        };
        match self {
            Self::Wide => {
                design.ball.iris_radius = 0.005_9;
                design.ball.pupil = 0.26;
            }
            Self::Hooded => {
                design.skin.crease = 0.004;
                design.skin.crease_depth = 0.002;
                design.skin.lid_thickness = 0.001_6;
            }
            Self::Natural | Self::Almond | Self::Round | Self::Downturned => {}
        }
    }
}

/// A named iris colour: the iris's outer, inner and fibre colours, measured from the range of real irises.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IrisColour {
    /// Warm gold-brown, lighter round the pupil (the default).
    Amber,
    /// Dark brown, the commonest iris.
    Brown,
    /// Green-brown: a brown ring round the pupil fading to green-olive.
    Hazel,
    /// Green with a gold ring round the pupil.
    Green,
    /// Blue-grey with a lighter, paler middle.
    Blue,
    /// Steel grey.
    Grey,
}

impl IrisColour {
    /// Every colour, in the order a picker lists them.
    pub const ALL: [Self; 6] = [Self::Amber, Self::Brown, Self::Hazel, Self::Green, Self::Blue, Self::Grey];

    /// The outer, inner and fibre colours.
    #[must_use]
    pub fn colours(self) -> (Colour, Colour, Colour) {
        let (outer, inner, fibre) = match self {
            Self::Amber => (0x00ae_7a3e, 0x00cc_9c58, 0x00d4_a466),
            Self::Brown => (0x005a_3a22, 0x007a_5232, 0x008a_6038),
            Self::Hazel => (0x006a_6a3a, 0x00a0_7a3e, 0x00b0_8a48),
            Self::Green => (0x004a_6a44, 0x008a_8a4a, 0x009a_a060),
            Self::Blue => (0x003a_5a7e, 0x007a_98b0, 0x009a_b4c8),
            Self::Grey => (0x005e_6a72, 0x008e_989e, 0x00a8_b0b4),
        };
        (Colour::hex(outer), Colour::hex(inner), Colour::hex(fibre))
    }

    /// Gives `design`'s iris these colours; its pattern keeps its values.
    pub fn apply(self, design: &mut EyeDesign) {
        (design.iris.outer, design.iris.inner, design.iris.fibre_colour) = self.colours();
    }
}

impl Validate for EyeDesign {
    fn check(&self, c: &mut Check) {
        c.range_f32("style", self.style, 0.0, 1.0);
        c.nested("ball", &self.ball);
        c.nested("iris", &self.iris);
        c.nested("sclera", &self.sclera);
        c.nested("aperture", &self.aperture);
        c.nested("skin", &self.skin);
        c.nested("lashes", &self.lashes);
        c.nested("anime", &self.anime);
        c.rule(&["paint"], self.paint.len() <= MAX_MARKS, "an eye holds at most 4096 painted marks");
        for (i, mark) in self.paint.iter().enumerate() {
            c.nested(&format!("paint/{i}"), mark);
        }
    }
}

/// A count's range check: counts are whole numbers, checked as numbers so their limits are listed like any other.
fn count(c: &mut Check, name: &str, value: u32, hi: u32) {
    c.range(name, f64::from(value), 0.0, f64::from(hi));
}

/// The eyeball's size and the cornea.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Ball {
    /// The eyeball's radius, metres (an adult's is about 12 mm).
    pub radius: f32,
    /// The iris's radius, metres (about 5.9 mm).
    pub iris_radius: f32,
    /// How far the cornea's apex stands past the eyeball's sphere, in eyeball radii.
    pub cornea_rise: f32,
    /// The pupil's radius, in iris radii.
    pub pupil: f32,
}

impl Default for Ball {
    fn default() -> Self {
        Self { radius: 0.012, iris_radius: 0.0059, cornea_rise: 0.09, pupil: 0.23 }
    }
}

impl Validate for Ball {
    fn check(&self, c: &mut Check) {
        c.range_f32("radius", self.radius, 0.008, 0.016);
        c.range_f32("iris_radius", self.iris_radius, 0.003, 0.008);
        c.rule(
            &["iris_radius", "radius"],
            self.iris_radius < self.radius * 0.7,
            "the iris's radius must be less than 0.7 of the eyeball's",
        );
        c.range_f32("cornea_rise", self.cornea_rise, 0.0, 0.2);
        c.range_f32("pupil", self.pupil, 0.05, 0.8);
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
    pub fibre_colour: Colour,
    /// The limbal ring's colour.
    pub limbal_colour: Colour,
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
            outer: Colour::hex(0x00ae_7a3e),
            inner: Colour::hex(0x00cc_9c58),
            deep: Colour::hex(0x0024_140a),
            fibre_colour: Colour::hex(0x00d4_a466),
            limbal_colour: Colour::hex(0x001e_120a),
            fibres: 0.8,
            collarette: 0.42,
            crypts: 40,
            specks: 25,
            furrows: 4,
            limbal_ring: 0.85,
        }
    }
}

impl Validate for Iris {
    fn check(&self, c: &mut Check) {
        c.range_f32("fibres", self.fibres, 0.0, 1.0);
        c.range_f32("collarette", self.collarette, 0.1, 0.9);
        count(c, "crypts", self.crypts, 200);
        count(c, "specks", self.specks, 400);
        count(c, "furrows", self.furrows, 12);
        c.range_f32("limbal_ring", self.limbal_ring, 0.0, 1.0);
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
        Self { white: Colour::hex(0x00c8_c6c2), edge: Colour::hex(0x00b8_a8a2), vessels: 15, network: 0.7 }
    }
}

impl Validate for Sclera {
    fn check(&self, c: &mut Check) {
        count(c, "vessels", self.vessels, 300);
        c.range_f32("network", self.network, 0.0, 1.0);
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
    /// The inner corner's height.
    pub inner_y: f32,
    /// The outer corner's height.
    pub outer_y: f32,
    /// How high the upper lid rises above the line between the corners, metres.
    pub upper: f32,
    /// Where along that line the upper lid peaks, from 0 (outer corner) to 1 (inner corner).
    pub upper_peak: f32,
    /// How far the lower lid drops below the line between the corners, metres.
    pub lower: f32,
    /// Where along that line the lower lid is lowest, from 0 (outer corner) to 1 (inner corner).
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

impl Validate for Aperture {
    fn check(&self, c: &mut Check) {
        c.range_f32("inner_x", self.inner_x, 0.008, 0.018);
        c.range_f32("outer_x", self.outer_x, -0.018, -0.008);
        c.range_f32("inner_y", self.inner_y, -0.006, 0.006);
        c.range_f32("outer_y", self.outer_y, -0.006, 0.006);
        c.range_f32("upper", self.upper, 0.0005, 0.012);
        c.range_f32("upper_peak", self.upper_peak, 0.15, 0.85);
        c.range_f32("lower", self.lower, 0.0005, 0.008);
        c.range_f32("lower_peak", self.lower_peak, 0.15, 0.85);
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
    /// The lid's thickness at its edge, metres: the band between the eye and the lash roots.
    pub lid_thickness: f32,
    /// How far above the upper lid's edge its crease runs, metres.
    pub crease: f32,
    /// How deep the crease folds, metres.
    pub crease_depth: f32,
}

impl Default for Skin {
    fn default() -> Self {
        Self {
            tone: Colour::hex(0x0088_5c44),
            freckles: 90.0,
            freckle: Colour::hex(0x0070_3a24),
            lid_thickness: 0.0012,
            crease: 0.0058,
            crease_depth: 0.001,
        }
    }
}

impl Validate for Skin {
    fn check(&self, c: &mut Check) {
        c.range_f32("freckles", self.freckles, 0.0, 200.0);
        c.range_f32("lid_thickness", self.lid_thickness, 0.0005, 0.004);
        c.range_f32("crease", self.crease, 0.002, 0.012);
        c.range_f32("crease_depth", self.crease_depth, 0.0, 0.004);
    }
}

/// The lashes: strands rooted along the lids' front edges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lashes {
    /// How many on the upper lid.
    pub upper: u32,
    /// How many on the lower lid.
    pub lower: u32,
    /// The longest upper lash, metres.
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
    /// Clumps placed by hand, on top of the lashes the counts above grow (set the counts to 0 to draw only these):
    /// each one where it is wanted along its lid, its own length, curl and sweep. At most 400.
    pub clumps: Vec<LashClump>,
}

/// One clump of lashes placed by hand on a lid: a few hairs rooted side by side that gather to one tip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LashClump {
    /// On the upper lid (`true`) or the lower.
    pub upper: bool,
    /// Where along the lid, 0 (the outer corner) to 1 (the inner).
    pub at: f32,
    /// How many hairs, 1 to 40.
    pub strands: u32,
    /// How far apart its outermost roots are along the lid, metres (0 to 3 mm).
    #[serde(default)]
    pub spread: f32,
    /// How long, metres (0 to 20 mm).
    pub length: f32,
    /// How far it curls up off the eye, 0 to 2 (1 as the grown lashes curl).
    #[serde(default = "full")]
    pub curl: f32,
    /// How far it sweeps along the lid, degrees from straight out: positive toward the outer corner, -80 to 80.
    #[serde(default)]
    pub sweep: f32,
}

impl Validate for LashClump {
    fn check(&self, c: &mut Check) {
        c.range_f32("at", self.at, 0.0, 1.0);
        c.range("strands", f64::from(self.strands), 1.0, 40.0);
        c.range_f32("spread", self.spread, 0.0, 0.003);
        c.range_f32("length", self.length, 0.0, 0.02);
        c.range_f32("curl", self.curl, 0.0, 2.0);
        c.range_f32("sweep", self.sweep, -80.0, 80.0);
    }
}

impl Default for Lashes {
    fn default() -> Self {
        Self {
            upper: 190,
            lower: 120,
            upper_length: 0.0062,
            lower_length: 0.0042,
            curl: 1.0,
            clumping: 0.85,
            thickness: 0.000_11,
            colour: Colour::hex(0x0016_0f0c),
            clumps: Vec::new(),
        }
    }
}

impl Validate for Lashes {
    fn check(&self, c: &mut Check) {
        count(c, "upper", self.upper, 600);
        count(c, "lower", self.lower, 300);
        c.range_f32("upper_length", self.upper_length, 0.0, 0.016);
        c.range_f32("lower_length", self.lower_length, 0.0, 0.01);
        c.range_f32("curl", self.curl, 0.0, 1.0);
        c.range_f32("clumping", self.clumping, 0.0, 1.0);
        c.range_f32("thickness", self.thickness, 0.000_02, 0.000_3);
        c.rule(&["clumps"], self.clumps.len() <= 400, "a design holds at most 400 placed clumps");
        for (i, clump) in self.clumps.iter().enumerate() {
            c.nested(&format!("clumps/{i}"), clump);
        }
    }
}

/// The anime style's painted iris and inked lines (used as `style` rises toward 1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Anime {
    /// The iris's colour at its top, under the lid's shadow.
    pub top: Colour,
    /// Its colour across the middle.
    pub mid: Colour,
    /// Its colour at the bottom, where the light pools.
    pub low: Colour,
    /// The ring round its rim.
    pub ring: Colour,
    /// The pupil's colour.
    pub pupil: Colour,
    /// How large the painted highlights are, `0..=1` (0: none).
    pub highlight: f32,
    /// The upper lash line's thickness at its thickest (toward the outer corner), metres.
    pub lash_line: f32,
    /// How far the lash line's wing reaches past the outer corner, metres.
    pub wing: f32,
    /// How strongly the lower lash line shows, `0..=1`.
    pub lower_line: f32,
    /// How strongly the double-lid line above the upper lid shows, `0..=1`.
    pub crease_line: f32,
    /// The lines' ink colour.
    pub ink: Colour,
}

impl Default for Anime {
    fn default() -> Self {
        Self {
            top: Colour::hex(0x0016_4a3a),
            mid: Colour::hex(0x002c_8a66),
            low: Colour::hex(0x0078_d8a8),
            ring: Colour::hex(0x000c_2a22),
            pupil: Colour::hex(0x0008_1c16),
            highlight: 0.7,
            lash_line: 0.0009,
            wing: 0.0022,
            lower_line: 0.5,
            crease_line: 0.6,
            ink: Colour::hex(0x0024_1612),
        }
    }
}

impl Validate for Anime {
    fn check(&self, c: &mut Check) {
        c.range_f32("highlight", self.highlight, 0.0, 1.0);
        c.range_f32("lash_line", self.lash_line, 0.0, 0.002);
        c.range_f32("wing", self.wing, 0.0, 0.006);
        c.range_f32("lower_line", self.lower_line, 0.0, 1.0);
        c.range_f32("crease_line", self.crease_line, 0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_eye_is_valid_and_an_empty_document_is_the_default() {
        EyeDesign::default().validate().unwrap();
        EyeDesign::anime().validate().unwrap();
        for shape in EyeShape::ALL {
            for colour in IrisColour::ALL {
                let mut design = EyeDesign::anime();
                shape.apply(&mut design);
                colour.apply(&mut design);
                design.validate().unwrap();
                let mut design = EyeDesign::default();
                shape.apply(&mut design);
                colour.apply(&mut design);
                design.validate().unwrap();
            }
        }
        let read: EyeDesign = serde_json::from_str("{}").unwrap();
        assert_eq!(read, EyeDesign::default());
    }

    #[test]
    fn every_bad_field_is_named_by_its_path() {
        let mut eye = EyeDesign::default();
        eye.ball.iris_radius = 0.0079;
        eye.ball.radius = 0.009;
        eye.iris.crypts = 900;
        eye.aperture.upper = 0.0;
        eye.lashes.thickness = f32::NAN;
        let errors = eye.validate().unwrap_err();
        let paths: Vec<&str> = errors.0.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["ball/iris_radius", "iris/crypts", "aperture/upper", "lashes/thickness"]);
        assert!(errors.0[0].message.contains("0.7 of the eyeball"), "{}", errors.0[0].message);
    }

    #[test]
    fn every_numeric_field_has_a_range() {
        let ranges = EyeDesign::default().ranges();
        // Every field but the seed and the colours: the style, then 4 + 6 + 2 + 8 + 4 + 7 + 5.
        assert_eq!(ranges.len(), 37, "{ranges:?}");
        let rule = ranges.iter().find(|r| r.path == "ball/radius").unwrap();
        assert!(rule.note.is_some(), "the iris rule is listed with the radius");
    }
}
