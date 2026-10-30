//! The eye's fine detail (#2465 to #2468): the iris's structure, extra catchlights, the sclera's corners and lids, and
//! strand lashes. Every part is off by default (a strength or a count of zero), so a face that never sets them paints
//! exactly as before; the eye presets turn them on.
//!
//! Sizes are fractions of the iris's radii (iris parts) or of the eye's half-width (lash strands), so one design reads
//! the same at any eye scale. Colours with alpha are tints laid over what is under them: the default fibre colours
//! are translucent white and black, so they suit any iris colour and both eyes of a heterochromatic pair.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::CatchlightShape;
use crate::check::{Check, Validate};
use crate::palette::ColourRef;

/// A whole-number field must be within `lo..=hi`. Checked as a rule, not a range: a range is offered to tools as a
/// slider over real numbers, and a count only takes whole ones.
fn count(c: &mut Check, name: &str, value: u32, lo: u32, hi: u32) {
    c.rule(
        &[name],
        (lo..=hi).contains(&value),
        &format!("{value} is out of range: use a whole number from {lo} to {hi}"),
    );
}

/// How many extra catchlights an eye may carry.
pub const MAX_CATCHLIGHTS: usize = 8;

// ------------------------------------------------------------------------------------------------ iris

/// The iris's structure (#2465), painted over its gradient and under its catchlights.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct IrisDetail {
    /// Radial fibres from the pupil out to the rim.
    pub fibres: IrisFibres,
    /// The toothed ring part way out (the collarette), where the inner and outer iris meet.
    pub collarette: Collarette,
    /// Small dark flecks scattered over the iris.
    pub flecks: IrisFlecks,
    /// The dark band at the iris's rim (the limbal ring).
    pub limbal: LimbalRing,
    /// A soft glow of the iris's lit colour around the pupil, 0 none.
    pub pupil_glow: f64,
}

impl Validate for IrisDetail {
    fn check(&self, c: &mut Check) {
        c.nested("fibres", &self.fibres);
        c.nested("collarette", &self.collarette);
        c.nested("flecks", &self.flecks);
        c.nested("limbal", &self.limbal);
        c.range("pupil_glow", self.pupil_glow, 0.0, 1.0);
    }
}

/// Radial fibres: thin light and dark wedges from the pupil toward the rim, wavering a little, seeded by
/// [`EyeStyle::seed`](super::EyeStyle::seed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct IrisFibres {
    /// Opacity, 0 none to 1 full.
    pub strength: f64,
    /// How many fibres go round the iris (half light, half dark).
    pub count: u32,
    /// Where they start, as a fraction of the iris radius from its centre.
    pub inner: f64,
    /// Where they end, as a fraction of the iris radius.
    pub outer: f64,
    /// Width of a fibre at its widest, as a fraction of the gap between neighbours.
    pub width: f64,
    /// How much each fibre bends and varies in length, 0 straight and even to 1 wild.
    pub waviness: f64,
    /// The light fibres' tint.
    pub light: ColourRef,
    /// The dark fibres' tint.
    pub dark: ColourRef,
}

impl Default for IrisFibres {
    fn default() -> Self {
        Self {
            strength: 0.0,
            count: 72,
            inner: 0.3,
            outer: 0.96,
            width: 0.5,
            waviness: 0.35,
            light: ColourRef::rgba(255, 255, 255, 110),
            dark: ColourRef::rgba(0, 0, 0, 90),
        }
    }
}

impl Validate for IrisFibres {
    fn check(&self, c: &mut Check) {
        c.range("strength", self.strength, 0.0, 1.0);
        count(c, "count", self.count, 4, 256);
        c.range("inner", self.inner, 0.0, 0.9);
        c.range("outer", self.outer, 0.1, 1.0);
        c.rule(&["inner", "outer"], self.inner < self.outer, "the fibres must start inside where they end");
        c.range("width", self.width, 0.05, 1.0);
        c.range("waviness", self.waviness, 0.0, 1.0);
    }
}

/// The collarette: a toothed ring where the inner iris meets the outer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Collarette {
    /// Opacity, 0 none to 1 full.
    pub strength: f64,
    /// Its mean radius, as a fraction of the iris radius.
    pub radius: f64,
    /// How many teeth it has round the iris.
    pub teeth: u32,
    /// How far the teeth reach in and out, as a fraction of the iris radius.
    pub depth: f64,
    /// How much the teeth vary from each other, 0 even to 1 ragged.
    pub irregularity: f64,
    /// Line width, as a fraction of the iris radius.
    pub width: f64,
    /// Line colour.
    pub colour: ColourRef,
}

impl Default for Collarette {
    fn default() -> Self {
        Self {
            strength: 0.0,
            radius: 0.5,
            teeth: 18,
            depth: 0.06,
            irregularity: 0.5,
            width: 0.022,
            colour: ColourRef::rgba(255, 244, 214, 120),
        }
    }
}

impl Validate for Collarette {
    fn check(&self, c: &mut Check) {
        c.range("strength", self.strength, 0.0, 1.0);
        c.range("radius", self.radius, 0.2, 0.9);
        count(c, "teeth", self.teeth, 3, 64);
        c.range("depth", self.depth, 0.0, 0.2);
        c.range("irregularity", self.irregularity, 0.0, 1.0);
        c.range("width", self.width, 0.005, 0.15);
    }
}

/// Flecks: small dark spots (crypts and freckles of the iris), placed by the seed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct IrisFlecks {
    /// Opacity, 0 none to 1 full.
    pub strength: f64,
    /// How many.
    pub count: u32,
    /// Their mean radius, as a fraction of the iris radius.
    pub size: f64,
    /// Their colour.
    pub colour: ColourRef,
}

impl Default for IrisFlecks {
    fn default() -> Self {
        Self { strength: 0.0, count: 10, size: 0.045, colour: ColourRef::rgba(30, 18, 10, 150) }
    }
}

impl Validate for IrisFlecks {
    fn check(&self, c: &mut Check) {
        c.range("strength", self.strength, 0.0, 1.0);
        count(c, "count", self.count, 0, 64);
        c.range("size", self.size, 0.01, 0.2);
    }
}

/// The limbal ring: the iris darkening toward its rim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LimbalRing {
    /// Opacity at the rim, 0 none to 1 full.
    pub strength: f64,
    /// How far in it reaches, as a fraction of the iris radius.
    pub width: f64,
    /// Its colour at the rim.
    pub colour: ColourRef,
}

impl Default for LimbalRing {
    fn default() -> Self {
        Self { strength: 0.0, width: 0.22, colour: ColourRef::rgba(14, 18, 24, 255) }
    }
}

impl Validate for LimbalRing {
    fn check(&self, c: &mut Check) {
        c.range("strength", self.strength, 0.0, 1.0);
        c.range("width", self.width, 0.02, 0.6);
    }
}

// ------------------------------------------------------------------------------------------------ catchlights

/// One extra catchlight (#2466), drawn over the iris after the main one and sliding with the view like it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Catchlight {
    /// Its shape.
    pub shape: CatchlightShape,
    /// Where it sits, in iris radii from the iris centre (x toward the character's left, y up), in both eyes alike:
    /// one light makes one reflection in each, as the main catchlight does.
    pub offset: [f64; 2],
    /// Its width as a fraction of the iris width.
    pub size: f64,
    /// Its height over its width.
    pub aspect: f64,
    /// Its turn, in degrees counter-clockwise.
    pub angle_deg: f64,
    /// Opacity.
    pub opacity: f64,
    /// Colour.
    pub colour: ColourRef,
}

impl Default for Catchlight {
    fn default() -> Self {
        Self {
            shape: CatchlightShape::Blob,
            offset: [-0.3, -0.4],
            size: 0.14,
            aspect: 1.0,
            angle_deg: 0.0,
            opacity: 0.8,
            colour: ColourRef::rgb(255, 255, 255),
        }
    }
}

impl Validate for Catchlight {
    fn check(&self, c: &mut Check) {
        c.range("offset/0", self.offset[0], -1.2, 1.2);
        c.range("offset/1", self.offset[1], -1.2, 1.2);
        c.range("size", self.size, 0.01, 1.0);
        c.range("aspect", self.aspect, 0.1, 4.0);
        c.range("angle_deg", self.angle_deg, -180.0, 180.0);
        c.range("opacity", self.opacity, 0.0, 1.0);
    }
}

// ------------------------------------------------------------------------------------------------ lashes

/// Strand lashes (#2468): individual tapered lashes along the upper and the lower lid, on top of the lash line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LashStrands {
    /// The upper lid's lashes.
    pub upper: StrandRow,
    /// The lower lid's lashes.
    pub lower: StrandRow,
}

impl Default for LashStrands {
    fn default() -> Self {
        Self {
            upper: StrandRow::default(),
            lower: StrandRow {
                length: 0.16,
                curl: 0.15,
                lift: 0.3,
                thickness: 0.0007,
                from: -0.2,
                ..StrandRow::default()
            },
        }
    }
}

impl Validate for LashStrands {
    fn check(&self, c: &mut Check) {
        c.nested("upper", &self.upper);
        c.nested("lower", &self.lower);
    }
}

/// One row of strand lashes. Lengths are in the eye's half-widths, so lashes grow with the eye.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StrandRow {
    /// How many lashes, 0 none.
    pub count: u32,
    /// The longest lash's length, in eye half-widths.
    pub length: f64,
    /// How much each lash curls away from the eye along its length, 0 straight to 1 a hook.
    pub curl: f64,
    /// How much the lashes lean toward the outer corner and grow longer there, 0 even to 1 a strong wing.
    pub lift: f64,
    /// How much neighbouring lashes gather into clumps, 0 none to 1 tight clumps.
    pub clumping: f64,
    /// Width at the root, in metres.
    pub thickness: f64,
    /// How much lashes vary in length and angle, 0 even to 1 wild.
    pub randomness: f64,
    /// Where the row starts along the lid, from the inner corner (-1) to the outer (1); it always runs to the outer
    /// corner.
    pub from: f64,
    /// Opacity.
    pub opacity: f64,
}

impl Default for StrandRow {
    fn default() -> Self {
        Self {
            count: 0,
            length: 0.34,
            curl: 0.5,
            lift: 0.65,
            clumping: 0.5,
            thickness: 0.0013,
            randomness: 0.5,
            from: -0.55,
            opacity: 1.0,
        }
    }
}

impl Validate for StrandRow {
    fn check(&self, c: &mut Check) {
        count(c, "count", self.count, 0, 200);
        c.range("length", self.length, 0.02, 1.0);
        c.range("curl", self.curl, 0.0, 1.0);
        c.range("lift", self.lift, 0.0, 1.0);
        c.range("clumping", self.clumping, 0.0, 1.0);
        c.range("thickness", self.thickness, 0.0001, 0.005);
        c.range("randomness", self.randomness, 0.0, 1.0);
        c.range("from", self.from, -1.0, 0.9);
        c.range("opacity", self.opacity, 0.0, 1.0);
    }
}
