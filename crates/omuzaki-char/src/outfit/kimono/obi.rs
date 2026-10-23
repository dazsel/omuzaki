//! The obi (#212): a wide sash wrapped round the kimono's waist, tied at the back in a drum (taiko) or a butterfly
//! bow, with an optional cord (obijime) round its middle knotted at the front.
//!
//! The sash cinches the kimono: it is knitted into the kimono's cloth rather than laid over it, its underside at the
//! lining and its top a little proud of the outside, so it comes no nearer the arms than the cloth does. The bow and
//! the knot stand off the sash at the back and the front, where the arms never are.

use std::f64::consts::PI;

use omuzaki_face_api::{Check, ColourRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kit::curve::{TAU, fraction, lerp};
use crate::kit::grid::MeshBuilder;
use crate::outfit::coat::edge::{Band, Patch, Tube, band, cord, loop_tube, patch};
use crate::outfit::coat::shell::Frame;
use crate::outfit::common::GarmentError;

/// The bow at the back.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BowStyle {
    /// None: the sash only.
    None,
    /// A drum: a flat padded fold hanging over the back, from above the sash to below it.
    #[default]
    Taiko,
    /// A butterfly: two wings out to the sides from a knot in the middle.
    Butterfly,
}

/// An obi.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Obi {
    /// Whether there is one.
    pub on: bool,
    /// Its middle's height, as a fraction of the torso.
    pub at: f64,
    /// Its height, as a fraction of the torso.
    pub height: f64,
    /// How far it stands proud of the kimono, metres.
    pub proud: f64,
    /// Its colour.
    pub colour: ColourRef,
    /// The bow at the back.
    pub bow: BowStyle,
    /// The cord round its middle, if any: its colour.
    pub cord: Option<ColourRef>,
}

impl Default for Obi {
    fn default() -> Self {
        Self {
            on: true,
            at: 0.3,
            height: 0.24,
            proud: 0.0012,
            colour: ColourRef::rgb(0xb0, 0x8a, 0x52),
            bow: BowStyle::Taiko,
            cord: Some(ColourRef::rgb(0x9a, 0x4a, 0x48)),
        }
    }
}

impl Obi {
    /// Checks the obi, its fields under `obi/`.
    pub fn check(&self, c: &mut Check) {
        c.range("obi/at", self.at, 0.1, 0.7);
        c.range("obi/height", self.height, 0.08, 0.4);
        c.range("obi/proud", self.proud, 0.0004, 0.004);
    }

    /// The open borders its pieces add: the bow's patches.
    pub fn borders(&self) -> usize {
        if !self.on {
            return 0;
        }
        match self.bow {
            BowStyle::None => 0,
            BowStyle::Taiko => 2,
            BowStyle::Butterfly => 6,
        }
    }

    /// Its bottom and top heights on `frame`.
    pub fn span(&self, frame: &Frame) -> (f64, f64) {
        let len = frame.column.length();
        let middle = frame.torso(self.at);
        (middle - self.height * len / 2.0, middle + self.height * len / 2.0)
    }
}

/// Points round the sash.
const AROUND: usize = 192;
/// How far the sash's underside stands off the lining, metres.
const UNDER: f64 = 0.0002;
/// How far the bow stands off the sash at least, metres.
const BOW_OFF: f64 = 0.006;
/// The drum's half width round the back (radians), how far it reaches above the sash and below it (fractions of the
/// sash's height), and how much it swells in the middle (metres).
const TAIKO: (f64, f64, f64, f64) = (0.55, 0.25, 0.55, 0.012);
/// A butterfly's wings: how far round they reach (radians), their height at the tip (a fraction of the sash's), how
/// far they swell out (metres); the knot's half width (radians) and height (a fraction of the sash's).
const BUTTERFLY: (f64, f64, f64, f64, f64) = (0.9, 1.5, 0.018, 0.09, 0.7);
/// The gap between pieces lying on one another, metres.
const GAP: f64 = 0.0003;
/// Grid of the bow's patches.
const BOW_GRID: (usize, usize) = (12, 10);
/// The cord's radius and its knot's, metres; the knot's half width round the front (radians).
const CORD: (f64, f64, f64) = (0.0011, 0.0024, 0.07);

/// Adds `obi` to `mesh` on the kimono `frame` (colours linear RGB: the sash's and the cord's, the sash's underside
/// the lining's). Returns the open borders added.
///
/// # Errors
/// The surface's or the kit's.
pub fn build(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    obi: &Obi,
    colour: [f32; 3],
    cord_colour: Option<[f32; 3]>,
    lining: [f32; 3],
) -> Result<usize, GarmentError> {
    if !obi.on {
        return Ok(0);
    }
    let (bottom, top) = obi.span(frame);
    let height = top - bottom;
    let middle = f64::midpoint(bottom, top);
    let under = UNDER - frame.cut.thickness;
    let thickness = frame.cut.thickness + obi.proud - UNDER;
    let surface = |k: usize, dy: f64| frame.on(middle + dy, TAU * fraction(k, AROUND), under);
    band(mesh, &Band { around: AROUND, surface: &surface, height, thickness, colour, ribs: 0 })?;
    // The sash's top, where the bow and the cord stand from.
    let over = under + thickness;
    if let Some(cord_colour) = cord_colour {
        let (radius, knot, half) = CORD;
        let (mut points, mut skins) = (Vec::with_capacity(AROUND), Vec::with_capacity(AROUND));
        for k in 0..AROUND {
            let (p, _, skin) = frame.on(middle, TAU * fraction(k, AROUND), over + GAP + radius)?;
            points.push(p);
            skins.push(skin);
        }
        let axis = |p: omuzaki_core::glam::DVec3| frame.centre(p.y);
        loop_tube(mesh, &Tube { points: &points, skins: &skins, radius, colour: cord_colour }, &axis)?;
        let (mut points, mut skins) = (Vec::new(), Vec::new());
        for k in 0..=12 {
            let (p, _, skin) =
                frame.on(middle, lerp(-half, half, fraction(k, 12)), over + 2.0 * (GAP + radius) + knot)?;
            points.push(p);
            skins.push(skin);
        }
        cord(mesh, &Tube { points: &points, skins: &skins, radius: knot, colour: cord_colour })?;
    }
    let off = over + BOW_OFF;
    match obi.bow {
        BowStyle::None => {}
        BowStyle::Taiko => {
            let (half, above, below, swell) = TAIKO;
            let (from, to) = (bottom - below * height, top + above * height);
            bow_patch(mesh, frame, colour, lining, |s, t| {
                let phi = PI + lerp(-half, half, t);
                let y = lerp(from, to, s);
                let lift = off + swell * (PI * s).sin() * (PI * t).sin();
                (y, phi, lift)
            })?;
        }
        BowStyle::Butterfly => {
            let (reach, tip, swell, knot, knot_height) = BUTTERFLY;
            for side in [1.0, -1.0] {
                bow_patch(mesh, frame, colour, lining, |s, t| {
                    // From the knot out to the tip, widening.
                    let phi = PI + side * lerp(knot * 0.5, reach, s);
                    let half = height / 2.0 * lerp(knot_height, tip, s);
                    let y = middle + lerp(-half, half, t);
                    let lift = off + swell * (PI * s).sin() * (1.0 - (2.0 * t - 1.0).powi(2));
                    (y, phi, lift)
                })?;
            }
            bow_patch(mesh, frame, colour, lining, |s, t| {
                let phi = PI + lerp(-knot, knot, t);
                let half = height / 2.0 * knot_height;
                let y = middle + lerp(-half, half, s);
                let lift = off + swell * 0.8 + 0.004 * (PI * t).sin();
                (y, phi, lift)
            })?;
        }
    }
    Ok(obi.borders())
}

/// Adds a bow's patch: `at(s, t)` the height, angle and lift of grid point `(s, t)`, both 0 to 1.
fn bow_patch(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    colour: [f32; 3],
    lining: [f32; 3],
    at: impl Fn(f64, f64) -> (f64, f64, f64),
) -> Result<(), GarmentError> {
    let (rows, cols) = BOW_GRID;
    let (mut points, mut normals, mut skins) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..rows {
        for j in 0..cols {
            let (y, phi, lift) = at(fraction(i, rows - 1), fraction(j, cols - 1));
            let (p, n, skin) = frame.on(y, phi, lift)?;
            points.push(p);
            normals.push(n);
            skins.push(skin);
        }
    }
    let piece = Patch {
        rows,
        cols,
        points,
        normals,
        skins,
        thickness: 0.003,
        colours: (colour, lining),
        edge: colour.map(|c| c * 0.85),
        uvs: None,
    };
    patch(mesh, &piece)
}
