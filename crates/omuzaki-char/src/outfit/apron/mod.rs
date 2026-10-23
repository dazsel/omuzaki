//! The `apron` garment (#394): a bib over the chest held by two straps that go over the shoulders and cross at the
//! back, a band round the waist tied in a bow at the back, and a panel hanging over the front of whatever is worn under
//! it, its hem gathered in a frill.
//!
//! It is laid out on the [coat framework](crate::outfit::coat)'s [`Frame`] without a shell of its own: the frame's
//! outside (over the dress or skirt under it, which the frame's skirt clears through their published reach) is where
//! every piece lies, each a lined [patch](crate::outfit::coat::edge::patch), a closed
//! [band](crate::outfit::coat::edge::band) or a cord.

use std::f64::consts::{FRAC_PI_2, PI};

use omuzaki_char_api::{BodyRegion, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::coat::edge::{Band, Patch, Tube, band, cord, loop_tube, patch};
use super::coat::hang::{Arms, Fall};
use super::coat::look::{self, CoatColours};
use super::coat::shell::{Frame, Skinning, Tones};
use super::coat::{CoatCut, Front, MAX_LENGTH, NeckShape, Neckline};
use super::common::{FabricStyle, Garment, GarmentCommon, GarmentError, GarmentOutput, LAYER_OUTER};
use crate::kit::curve::{TAU, fraction, lerp};
use crate::kit::grid::MeshBuilder;

/// The layer an apron is worn on: over a dress, a skirt or a vest, under a coat.
pub const LAYER_APRON: i32 = LAYER_OUTER - 6;
/// The cut of the frame's rings.
const EXPONENT: f64 = 2.2;
/// The frame's waist seam, as a fraction of the torso.
const WAIST: f64 = 0.3;
/// The rolled edges' lip, metres.
const LIP: f64 = 0.0006;
/// How far each piece stands off what it lies on, metres.
const STANDOFF: f64 = 0.0004;
/// Rows and columns of the bib and the panel.
const GRID: (usize, usize) = (14, 16);
/// Rows along a strap, and its columns.
const STRAP_GRID: (usize, usize) = (48, 3);
/// The frill's columns per metre of its width, and its rows.
const FRILL_GRID: (f64, usize) = (900.0, 5);
/// The frill's gathers per radian round the body.
const GATHERS: f64 = 14.0;
/// Points round the waist band.
const AROUND: usize = 160;
/// The straps' path: where they leave the bib (a fraction of its half width at the top), the height they cross the
/// shoulder at (a fraction of the way from the torso's top to the neck's), the angle they cross it at (radians from
/// the centre front), and how far round from the centre back they meet the band.
const STRAP: (f64, f64, f64, f64) = (0.8, 0.35, 1.05, 0.32);
/// The bow: its loops' half width round the back (radians) and half height (metres), the tails' length (metres), the
/// tie's radius (metres).
const BOW: (f64, f64, f64, f64) = (0.2, 0.022, 0.09, 0.0035);

/// The parameters of an apron.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ApronParams {
    /// The common settings: `ease` is how far it lies off what is under it, `fabric` the cloth.
    pub common: GarmentCommon,
    /// How thick the cloth is, metres.
    pub thickness: f64,
    /// Where the panel's hem is: 0 at the hips' joints, 1 at the ankles.
    pub length: f64,
    /// The panel's half width round the body, radians.
    pub width: f64,
    /// A bib over the chest, with straps.
    pub bib: bool,
    /// The bib's top, as a fraction of the torso.
    pub bib_top: f64,
    /// The bib's half width round the body at its top, radians.
    pub bib_width: f64,
    /// The straps' width, metres.
    pub strap_width: f64,
    /// The frill at the hem: its depth, metres (0 for none).
    pub frill: f64,
    /// The waist band's height, metres.
    pub band: f64,
    /// A bow at the back.
    pub bow: bool,
    /// The lining, the rolled edges and the toon.
    pub colours: CoatColours,
}

impl Default for ApronParams {
    fn default() -> Self {
        Self {
            common: GarmentCommon {
                ease: 0.004,
                fabric: FabricStyle::Plain { colour: ColourRef::rgb(0xf6, 0xf3, 0xec) },
                ..GarmentCommon::default()
            },
            thickness: 0.0015,
            length: 0.3,
            width: 0.85,
            bib: true,
            bib_top: 0.72,
            bib_width: 0.42,
            strap_width: 0.018,
            frill: 0.03,
            band: 0.022,
            bow: true,
            colours: CoatColours {
                lining: ColourRef::rgb(0xea, 0xe6, 0xde),
                trim: ColourRef::rgb(0xd8, 0xd2, 0xc8),
                ..CoatColours::default()
            },
        }
    }
}

impl Validate for ApronParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        c.range("thickness", self.thickness, 0.0005, 0.004);
        c.range("length", self.length, -0.05, MAX_LENGTH);
        c.range("width", self.width, 0.3, 1.4);
        c.range("bib_top", self.bib_top, 0.45, 0.9);
        c.range("bib_width", self.bib_width, 0.2, 0.9);
        c.range("strap_width", self.strap_width, 0.006, 0.04);
        c.range("frill", self.frill, 0.0, 0.08);
        c.range("band", self.band, 0.01, 0.06);
    }
}

impl ApronParams {
    /// The frame's cut: a column over whatever is under it, its skirt hanging from the waist.
    pub fn cut(&self) -> CoatCut {
        CoatCut {
            exponent: EXPONENT,
            ease: self.common.ease,
            thickness: self.thickness,
            waist: WAIST,
            front: Front::Closed,
            neckline: Neckline { shape: NeckShape::High, depth: 0.0, width: 0.9, back: 0.0, rise: 0.0 },
            armhole: 0.0,
            length: self.length,
            back_drop: 0.0,
            fall: Fall::Drape,
            flare: 0.02,
            folds: (0, 0.0),
            arms: Arms::Clear { sleeve: (0.0, 0.0), margin: 0.006 },
            lip: LIP,
        }
    }
}

/// The apron part kind.
pub struct Apron;

impl PartKind for Apron {
    type Params = ApronParams;

    fn layer() -> Option<i32> {
        Some(LAYER_APRON)
    }

    fn meta() -> PartMeta {
        PartMeta {
            id: "apron",
            summary: "an apron over a dress or skirt: a bib with straps crossed at the back, a waist band tied in a bow, \
                      a front panel with a frilled hem",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn provenance() -> omuzaki_char_api::KindProvenance {
        omuzaki_char_api::KindProvenance::ProjectOwned
    }

    fn build(p: &ApronParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: GarmentError| ctx.fail(e.to_string());
        let frame = Frame::new(ctx.fit()?, &p.cut(), super::coat::under(ctx)).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let tones = look::tones(ctx, &p.common, &p.colours)?;
        let mut mesh = MeshBuilder::with_colours();
        pieces(&mut mesh, &frame, p, &tones).map_err(fail)?;
        let material = out.material(ctx, "apron", look::cloth(ctx, &p.colours, false)?)?;
        out.mesh(ctx, "apron", mesh, material)?;
        out.finish(ctx, &p.common, &Self::regions(p), EXPONENT)
    }
}

/// Every piece of `p` on `frame`.
fn pieces(mesh: &mut MeshBuilder, frame: &Frame, p: &ApronParams, tones: &Tones) -> Result<(), GarmentError> {
    let waist = frame.waist();
    let lift = STANDOFF;
    let half_band = p.band / 2.0;
    // The panel: from the band down to the hem, widening with the skirt.
    sheet(mesh, frame, tones, GRID, |s, t| (lerp(waist, frame.hem, s), lerp(-p.width, p.width, t), lift))?;
    if p.frill > 0.0 {
        // Gathered over the panel's bottom.
        let depth = p.frill.min((waist - frame.hem) * 0.6);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small column count
        let cols = ((2.0 * p.width * radius(frame, frame.hem) * FRILL_GRID.0).ceil() as usize).clamp(24, 400);
        let gathers = (GATHERS * 2.0 * p.width).round();
        sheet(mesh, frame, tones, (FRILL_GRID.1, cols), |s, t| {
            let wave = 0.5 + 0.5 * (gathers * PI * t).cos();
            let y = lerp(frame.hem + depth, frame.hem + 0.001, s);
            (y, lerp(-p.width - 0.02, p.width + 0.02, t), lift + frame.cut.thickness + 0.001 + 0.004 * wave * s)
        })?;
    }
    if p.bib {
        let top = frame.torso(p.bib_top);
        let start = waist + half_band;
        sheet(mesh, frame, tones, GRID, |s, t| {
            let y = lerp(start, top, s);
            let half = lerp(p.bib_width * 1.15, p.bib_width, s);
            (y, lerp(-half, half, t), lift)
        })?;
        for side in [1.0, -1.0] {
            strap(mesh, frame, p, tones, side, top)?;
        }
    }
    // The band round the waist, over the panel, the bib and the straps' ends.
    let over = lift + frame.cut.thickness + 0.002;
    let surface = |k: usize, dy: f64| frame.on(waist + dy, TAU * fraction(k, AROUND), over);
    band(
        mesh,
        &Band { around: AROUND, surface: &surface, height: p.band, thickness: 0.002, colour: tones.outer, ribs: 0 },
    )?;
    if p.bow {
        bow(mesh, frame, tones, waist, over + 0.002 + BOW.3)?;
    }
    Ok(())
}

/// The radius of the frame's ring at `y`, metres.
fn radius(frame: &Frame, y: f64) -> f64 {
    super::coat::trims::radius(frame, y)
}

/// A lined patch lying on the frame: `at(s, t)` the height, angle and lift of grid point `(s, t)`, both 0 to 1.
fn sheet(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    tones: &Tones,
    (rows, cols): (usize, usize),
    at: impl Fn(f64, f64) -> (f64, f64, f64),
) -> Result<(), GarmentError> {
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
    patch(
        mesh,
        &Patch {
            rows,
            cols,
            points,
            normals,
            skins,
            thickness: frame.cut.thickness,
            colours: (tones.outer, tones.lining),
            edge: tones.edge,
            uvs: None,
        },
    )
}

/// A strap from the bib's top corner on `side` (1 the left, -1 the right) over the shoulder and down the back, crossing
/// the other at the back to meet the band on the far side.
#[allow(clippy::many_single_char_names, clippy::cast_precision_loss)] // the path's textbook names; a short list
fn strap(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    p: &ApronParams,
    tones: &Tones,
    side: f64,
    top: f64,
) -> Result<(), GarmentError> {
    let (leave, over, shoulder, meet) = STRAP;
    let crest = frame.column.top() + over * (frame.column.neck_top() - frame.column.top());
    let waist = frame.waist();
    // Key points of the path in (height, angle) on the left; the right mirrors them.
    let keys = [
        (top, p.bib_width * leave),
        (lerp(top, crest, 0.7), lerp(p.bib_width * leave, shoulder, 0.6)),
        (crest, shoulder),
        (crest, FRAC_PI_2 + 0.25),
        (crest, PI - shoulder),
        (lerp(crest, waist, 0.35), PI - 0.45),
        (waist, PI + meet),
    ];
    let (rows, cols) = STRAP_GRID;
    // The left strap crosses over the right at the back.
    let lift = STANDOFF + if side > 0.0 { frame.cut.thickness + 0.0015 } else { 0.0 };
    let path = |s: f64| {
        let x = s * (keys.len() - 1) as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // an index into a short list
        let k = (x.floor() as usize).min(keys.len() - 2);
        let f = x - k as f64;
        let f = f * f * (3.0 - 2.0 * f);
        (lerp(keys[k].0, keys[k + 1].0, f), lerp(keys[k].1, keys[k + 1].1, f))
    };
    let (mut points, mut normals, mut skins): (Vec<DVec3>, Vec<DVec3>, Vec<Skinning>) =
        (Vec::new(), Vec::new(), Vec::new());
    for i in 0..rows {
        let s = fraction(i, rows - 1);
        let (y, phi) = path(s);
        // Across the strap: round the body, a strap's width at that height.
        let half = p.strap_width / 2.0 / radius(frame, y);
        for j in 0..cols {
            let a = phi + lerp(-half, half, fraction(j, cols - 1));
            let a = if side > 0.0 { a } else { TAU - a };
            let (q, n, skin) = frame.on(y, a, lift)?;
            points.push(q);
            normals.push(n);
            skins.push(skin);
        }
    }
    patch(
        mesh,
        &Patch {
            rows,
            cols,
            points,
            normals,
            skins,
            thickness: frame.cut.thickness,
            colours: (tones.outer, tones.lining),
            edge: tones.edge,
            uvs: None,
        },
    )
}

/// The bow at the back of the band, `lift` off the frame: two loops either side of a knot, and two tails hanging.
fn bow(mesh: &mut MeshBuilder, frame: &Frame, tones: &Tones, waist: f64, lift: f64) -> Result<(), GarmentError> {
    const STEPS: usize = 32;
    let (half, height, tails, radius) = BOW;
    let axis = |q: DVec3| frame.centre(q.y);
    for side in [1.0, -1.0] {
        // A loop out to the side and back, round an ellipse on the back.
        let (mut points, mut skins) = (Vec::with_capacity(STEPS), Vec::with_capacity(STEPS));
        for k in 0..STEPS {
            let a = TAU * fraction(k, STEPS);
            let phi = PI + side * half * (0.55 + 0.45 * a.cos());
            let y = waist + height * a.sin() * (0.4 + 0.6 * (1.0 - a.cos()) / 2.0);
            let (q, _, skin) = frame.on(y, phi, lift + 0.004 * (1.0 + a.cos()) / 2.0)?;
            points.push(q);
            skins.push(skin);
        }
        loop_tube(mesh, &Tube { points: &points, skins: &skins, radius, colour: tones.outer }, &axis)?;
        // A tail hanging from the knot.
        let (mut points, mut skins) = (Vec::new(), Vec::new());
        for k in 0..=12 {
            let s = fraction(k, 12);
            let (q, _, skin) = frame.on(waist - tails * s, PI + side * (0.04 + 0.05 * s), lift)?;
            points.push(q);
            skins.push(skin);
        }
        cord(mesh, &Tube { points: &points, skins: &skins, radius, colour: tones.outer })?;
    }
    // The knot.
    let (mut points, mut skins) = (Vec::new(), Vec::new());
    for k in 0..=8 {
        let (q, _, skin) = frame.on(waist, PI + lerp(-0.07, 0.07, fraction(k, 8)), lift + radius + 0.002)?;
        points.push(q);
        skins.push(skin);
    }
    cord(mesh, &Tube { points: &points, skins: &skins, radius: radius * 1.6, colour: tones.outer })
}

impl Garment for Apron {
    fn common(p: &ApronParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &ApronParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso]
    }

    /// Two for each patch: the panel, the frill, the bib and each strap. The band, the bow and its tails are closed.
    fn open_borders(p: &ApronParams) -> usize {
        2 + if p.frill > 0.0 { 2 } else { 0 } + if p.bib { 6 } else { 0 }
    }
}

omuzaki_char_api::register_part_kind!(Apron);
