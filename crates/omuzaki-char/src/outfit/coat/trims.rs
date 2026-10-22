//! The pieces that sit on a coat's outside: lapels along an open front's V, pocket flaps and welts, a row of buttons,
//! a stitched line (a back vent, a seam). Each lies on the shell's outside ([`Frame::on`]) a little beyond the rolled
//! edges, so it never comes nearer the body than the cloth under it.

use std::f64::consts::PI;

use omuzaki_core::glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Front;
use super::edge::{Band, Patch, Tube, band, cord, patch};
use super::shell::{Frame, Skinning, Tones};
use crate::kit::curve::{TAU, fraction, lerp, sstep};
use crate::kit::grid::MeshBuilder;
use crate::outfit::common::GarmentError;
use crate::outfit::trim::Basis;
use crate::outfit::trim::buttons::{ButtonStyle, button};
use crate::outfit::trim::chain::{LINK, PITCH, add_links, links};
use crate::outfit::trim::clasp::{self, CLASP_SIZE, ClaspStyle};

/// How far a piece's underside stands beyond the shell's outside, over the rolled edge's lip, metres.
const STANDOFF: f64 = 0.0004;
/// A lapel's rows along the V and points across it.
const LAPEL_GRID: (usize, usize) = (28, 6);
/// A lapel starts where the front edges have opened this far either side of the centre, radians: above the overlap.
const LAPEL_START: f64 = 0.03;
/// The farthest round from the centre front a lapel reaches, radians.
const LAPEL_REACH: f64 = 0.8;
/// The narrowest a lapel is anywhere, as a share of its width: its point at the break.
const LAPEL_TIP: f64 = 0.06;
/// The notch: where along the lapel it starts and ends, and how much of the width it leaves.
const NOTCH: (f64, f64, f64) = (0.74, 0.84, 0.3);
/// The collar above the notch, as a share of the lapel's width.
const COLLAR: f64 = 0.72;
/// A pocket's points across and down.
const POCKET_GRID: (usize, usize) = (4, 7);
/// Steps up the height when looking for where the front has opened.
const SEARCH: usize = 200;

/// The lapels' cut.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LapelStyle {
    /// A notched lapel: a step between the lapel and the collar.
    #[default]
    Notch,
    /// A shawl lapel: one smooth curve up to the collar.
    Shawl,
    /// No lapels.
    None,
}

/// The lapels of an open front.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Lapel {
    /// The cut.
    pub style: LapelStyle,
    /// The widest it is, metres round the body.
    pub width: f64,
    /// How far its fold rolls out from the chest, metres.
    pub roll: f64,
}

impl Default for Lapel {
    fn default() -> Self {
        Self { style: LapelStyle::Notch, width: 0.045, roll: 0.002 }
    }
}

impl Lapel {
    /// Checks the lapel, its fields under `c`.
    pub fn check(&self, c: &mut omuzaki_face_api::Check) {
        c.range("width", self.width, 0.0, 0.12);
        c.range("roll", self.roll, 0.0, 0.01);
    }

    /// The lapel's width at `s`, from the break (0) up to the collar line (1), as a share of [`Lapel::width`].
    fn share(&self, s: f64) -> f64 {
        let base = s.powf(0.6);
        let w = match self.style {
            LapelStyle::Shawl => base * (1.0 - 0.3 * s.powi(6)),
            LapelStyle::Notch | LapelStyle::None => {
                let (start, end, depth) = NOTCH;
                let notch = sstep(start, start + 0.025, s) * (1.0 - sstep(end - 0.02, end, s));
                let collar = lerp(1.0, COLLAR, sstep(end - 0.02, end, s));
                base * collar * (1.0 - (1.0 - depth) * notch)
            }
        };
        w.max(LAPEL_TIP)
    }
}

/// The ring's mean radius at `y`: metres round the body to radians.
pub fn radius(frame: &Frame, y: f64) -> f64 {
    frame.ring(y).map_or(0.1, |r| f64::midpoint(r.radii.x, r.radii.y)).max(0.01)
}

/// Where the lapels break: at the V's start, or above it where the front edges have opened [`LAPEL_START`] either
/// side of the centre.
pub fn break_height(frame: &Frame) -> Option<f64> {
    let Front::Open { point, .. } = frame.cut.front else { return None };
    let (low, high) = (frame.waist().max(frame.torso(point)), frame.top - 0.01);
    (0..=SEARCH).map(|k| lerp(low, high, fraction(k, SEARCH))).find(|&y| frame.edge(y) >= LAPEL_START)
}

/// Adds the lapels of an open front to `mesh`, the top in the cloth's tone, the underside in the lining's, rolled in
/// the edge's; returns the open borders it added (two a lapel), 0 for none.
///
/// # Errors
/// The kit's.
#[allow(clippy::many_single_char_names)] // the grid's textbook names
pub fn lapels(mesh: &mut MeshBuilder, frame: &Frame, lapel: &Lapel, tones: &Tones) -> Result<usize, GarmentError> {
    if lapel.style == LapelStyle::None || lapel.width <= 0.0 {
        return Ok(0);
    }
    let Some(from) = break_height(frame) else { return Ok(0) };
    let to = frame.top - 0.003;
    if to - from < 0.01 {
        return Ok(0);
    }
    let (rows, cols) = LAPEL_GRID;
    let lift = frame.cut.lip + STANDOFF;
    let top = tones.outer.map(|c| c * 0.96);
    for side in [1.0, -1.0] {
        let (mut points, mut normals, mut skins) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..rows {
            let s = fraction(i, rows - 1);
            let y = lerp(from, to, s);
            let r = radius(frame, y);
            let edge = frame.edge(y) + 0.002 / r;
            // On a narrow body the lapel keeps to the chest, off the shoulders and the arms' roots.
            let width = (lapel.width / r).min(LAPEL_REACH - edge).max(0.0) * lapel.share(s);
            for j in 0..cols {
                let t = fraction(j, cols - 1);
                let phi = edge + t * width;
                let phi = if side > 0.0 { phi } else { TAU - phi };
                let (p, n, skin): (DVec3, DVec3, Skinning) = frame.on(y, phi, lift + lapel.roll * (1.0 - t))?;
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
            thickness: frame.cut.thickness,
            colours: (top, tones.lining),
            edge: tones.edge,
            uvs: None,
        };
        patch(mesh, &piece)?;
    }
    Ok(4)
}

/// A pocket: a flap (or a welt, when short) hanging from its top edge.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pocket {
    /// The top edge's middle: its height and its angle round the body.
    pub at: (f64, f64),
    /// Its width round the body and its height down it, metres.
    pub size: (f64, f64),
    /// How much it stands off the cloth at its bottom, metres (a flap's sag).
    pub sag: f64,
}

/// Adds `pocket` to `mesh`; returns the open borders it added (two).
///
/// # Errors
/// The kit's.
#[allow(clippy::many_single_char_names)] // the grid's textbook names
pub fn pocket(mesh: &mut MeshBuilder, frame: &Frame, pocket: &Pocket, tones: &Tones) -> Result<usize, GarmentError> {
    let (rows, cols) = POCKET_GRID;
    let (y, phi) = pocket.at;
    let (w, h) = pocket.size;
    let half = w / 2.0 / radius(frame, y);
    let lift = frame.cut.lip + STANDOFF;
    let (mut points, mut normals, mut skins) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..rows {
        let s = fraction(i, rows - 1);
        for j in 0..cols {
            let t = fraction(j, cols - 1);
            let (p, n, skin) = frame.on(y - s * h, phi - half + 2.0 * half * t, lift + pocket.sag * s)?;
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
        thickness: frame.cut.thickness,
        colours: (tones.outer.map(|c| c * 0.94), tones.lining),
        edge: tones.edge,
        uvs: None,
    };
    patch(mesh, &piece)?;
    Ok(2)
}

/// Adds a button of `style` at each `(height, angle)` of `at` to `builder`, on the outside.
///
/// # Errors
/// The surface's or the kit's.
pub fn buttons(
    builder: &mut MeshBuilder,
    frame: &Frame,
    at: &[(f64, f64)],
    style: &ButtonStyle,
) -> Result<(), GarmentError> {
    for &(y, phi) in at {
        let (foot, normal, skin) = frame.on(y, phi, frame.cut.lip)?;
        button(builder, foot, normal, style, &skin).map_err(|e| GarmentError::invalid("buttons", e.to_string()))?;
    }
    Ok(())
}

/// Adds a stitched line down the outside at angle `phi` from height `from` to `to`: a thin closed cord.
///
/// # Errors
/// The kit's.
pub fn stitch(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    phi: f64,
    (from, to): (f64, f64),
    colour: [f32; 3],
) -> Result<(), GarmentError> {
    const STEPS: usize = 24;
    const RADIUS: f64 = 0.0007;
    let mut points = Vec::with_capacity(STEPS + 1);
    let mut skins = Vec::with_capacity(STEPS + 1);
    for k in 0..=STEPS {
        let (p, _, skin) = frame.on(lerp(from, to, fraction(k, STEPS)), phi, frame.cut.lip * 0.5)?;
        points.push(p);
        skins.push(skin);
    }
    cord(mesh, &Tube { points: &points, skins: &skins, radius: RADIUS, colour })
}

/// The centre back's angle.
pub const BACK: f64 = PI;

/// A pair of clasps either side of an open front, joined by a chain that hangs between them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Clasps {
    /// Whether there are any.
    pub on: bool,
    /// Their height, as a fraction of the way from the waist seam (0) to the collar line (1).
    pub at: f64,
    /// Each one's ring diameter, metres.
    pub size: f64,
    /// What hangs under each ring.
    pub style: ClaspStyle,
    /// How far the chain sags below them, metres; 0 for none.
    pub sag: f64,
    /// Each link's size, metres.
    pub link: f64,
    /// A pendant at the chain's middle.
    pub pendant: bool,
}

impl Default for Clasps {
    fn default() -> Self {
        Self { on: true, at: 0.85, size: 0.022, style: ClaspStyle::Medallion, sag: 0.014, link: 0.0018, pendant: false }
    }
}

impl Clasps {
    /// Checks the clasps, their fields under `c`.
    pub fn check(&self, c: &mut omuzaki_face_api::Check) {
        c.range("at", self.at, 0.0, 1.0);
        c.range("size", self.size, 0.006, 0.08);
        c.range("sag", self.sag, 0.0, 0.2);
        c.range("link", self.link, 0.0008, 0.008);
    }
}

/// How far a clasp's hanging pieces stand off the cloth beyond the stud, metres per unit of scale.
const CLASP_STANDOFF: f64 = 0.002;
/// How many times a clasp is pushed out to clear the body.
const CLASP_TRIES: usize = 8;
/// How far in from each front edge a clasp's stud sits, radians.
const CLASP_IN: f64 = 0.06;

/// Adds `clasps` on the open front of `frame` to `metal`.
///
/// # Errors
/// The kit's.
#[allow(clippy::cast_possible_truncation)] // a share for a skin weight
pub fn clasps(metal: &mut MeshBuilder, frame: &Frame, clasps: &Clasps) -> Result<(), GarmentError> {
    let trim = |e: crate::outfit::trim::TrimError| GarmentError::invalid("clasps", e.to_string());
    if !clasps.on {
        return Ok(());
    }
    let y = lerp(frame.waist(), frame.top, clasps.at).min(frame.top - 0.012);
    let edge = frame.edge(y);
    let k = clasps.size / CLASP_SIZE;
    let mut studs = Vec::with_capacity(2);
    for phi in [edge + CLASP_IN, TAU - edge - CLASP_IN] {
        let (foot, normal, skin) = frame.on(y, phi, frame.cut.lip)?;
        let level = (normal - DVec3::Y * normal.y).try_normalize().unwrap_or(normal);
        let basis = clasp::frame(foot, DVec3::Y, level).map_err(trim)?;
        // Out from the cloth until every piece clears the body under the coat by the cloth's own stand-off: over the
        // front's opening the chest can come nearer than the cloth the clasp is sewn to.
        let least = frame.cut.ease + frame.cut.thickness;
        let mut standoff = CLASP_STANDOFF * k;
        let mut pieces = MeshBuilder::new();
        for _ in 0..CLASP_TRIES {
            pieces = MeshBuilder::new();
            clasp::pieces(&mut pieces, None, (&basis, &basis), clasps.style, (k, standoff), &skin).map_err(trim)?;
            let nearest =
                pieces.mesh().positions.iter().map(|p| outside(frame, p.as_dvec3())).fold(f64::INFINITY, f64::min);
            if nearest >= least {
                break;
            }
            standoff += (least - nearest).max(0.0005);
        }
        metal.merge(&pieces).map_err(|e| GarmentError::invalid("clasps", e.to_string()))?;
        studs.push((foot, level, skin));
    }
    if clasps.sag > 0.0 {
        let scale = clasps.link / LINK.0;
        let lift = (LINK.0 * 1.45 + LINK.1) * scale + 0.002;
        let out = (studs[0].1 + studs[1].1).try_normalize().unwrap_or(DVec3::Z);
        let hang = crate::outfit::trim::chain::Hang {
            from: studs[0].0 + studs[0].1 * lift,
            to: studs[1].0 + studs[1].1 * lift,
            up: DVec3::Y,
            out,
            sag: clasps.sag,
            bow: 0.004,
        };
        let chain = links(&hang, PITCH * scale, None, 0.002 * scale).map_err(trim)?;
        let n = chain.len();
        let skin = |k: usize| -> Skinning {
            let s = (fraction(k, n.max(1))).clamp(0.0, 1.0);
            crate::outfit::trousers::rows::mix_skin(&studs[0].2, &studs[1].2, s as f32)
        };
        add_links(metal, &chain, out, scale, &skin).map_err(trim)?;
        if clasps.pendant {
            let middle = hang.at(0.5);
            let basis = Basis { origin: middle, x: DVec3::Y.cross(out), y: DVec3::Y, z: out };
            let basis = Basis { origin: basis.place(DVec3::new(0.0, 0.0, 0.004)), ..basis };
            clasp::pendant_cross(metal, &basis, k, &skin(n / 2)).map_err(trim)?;
        }
    }
    Ok(())
}

/// A band laid along both front edges of an open front, from the hem to the collar line: a facing turned out, plain or
/// in a textured material.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FrontBand {
    /// Its width round the body, metres.
    pub width: f64,
    /// Its top's colour and its underside's (linear RGB), and its rolled edge's.
    pub colours: ([f32; 3], [f32; 3], [f32; 3]),
    /// Whether to give it texture coordinates: along it in widths, across it 0 to 1.
    pub textured: bool,
}

/// The rows of a front band are about this far apart, metres.
const BAND_ROW: f64 = 0.008;

/// Adds `band` to `mesh` along the front edges of `frame`; returns the open borders it added (two a side), none for a
/// closed front.
///
/// # Errors
/// The kit's.
#[allow(clippy::many_single_char_names)] // the grid's textbook names
pub fn front_band(mesh: &mut MeshBuilder, frame: &Frame, band: &FrontBand) -> Result<usize, GarmentError> {
    let Front::Open { .. } = frame.cut.front else { return Ok(0) };
    let (from, to) = (frame.hem_at(frame.edge(frame.hem)) + 0.003, frame.top - 0.003);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small row count
    let rows = (((to - from) / BAND_ROW).ceil() as usize).clamp(8, 240);
    let cols = 4;
    let lift = frame.cut.lip + STANDOFF;
    for side in [1.0, -1.0] {
        let (mut points, mut normals, mut skins, mut uvs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut along = 0.0;
        let mut last: Option<DVec3> = None;
        for i in 0..rows {
            let y = lerp(from, to, fraction(i, rows - 1));
            let r = radius(frame, y);
            let edge = frame.edge(y) + 0.002 / r;
            let mut row = Vec::with_capacity(cols);
            for j in 0..cols {
                let t = fraction(j, cols - 1);
                let phi = edge + t * band.width / r;
                let phi = if side > 0.0 { phi } else { TAU - phi };
                row.push((frame.on(y, phi, lift)?, t));
            }
            let middle = row[cols / 2].0.0;
            along += last.map_or(0.0, |p| p.distance(middle));
            last = Some(middle);
            for ((p, n, skin), t) in row {
                points.push(p);
                normals.push(n);
                skins.push(skin);
                uvs.push(omuzaki_core::glam::DVec2::new(along / band.width, if side > 0.0 { t } else { 1.0 - t }));
            }
        }
        let piece = Patch {
            rows,
            cols,
            points,
            normals,
            skins,
            thickness: frame.cut.thickness,
            colours: (band.colours.0, band.colours.1),
            edge: band.colours.2,
            uvs: band.textured.then_some(uvs),
        };
        patch(mesh, &piece)?;
    }
    Ok(4)
}

/// Points round a rib hem band.
const HEM_AROUND: usize = 288;
/// Ribs round a rib hem band.
const HEM_RIBS: usize = 72;

/// How far a rib band stands proud of the cloth it is knitted into, metres (its ribs stand further).
const RIB_PROUD: f64 = 0.0003;
/// How far a rib band's underside stands off the lining, metres.
const RIB_UNDER: f64 = 0.0002;

/// Adds a knitted rib band `height` tall round the hem of a closed coat, from the hem up: a closed [`band`] knitted in
/// place of the shell's cloth, as thick as the cloth and a little proud of it (a rib gathers the hem in rather than
/// standing out over it, so it comes no nearer the arms than the cloth). A coat with an open front has none.
///
/// # Errors
/// The surface's or the kit's.
pub fn hem_rib(mesh: &mut MeshBuilder, frame: &Frame, height: f64, colour: [f32; 3]) -> Result<(), GarmentError> {
    if !frame.closed() || height <= 0.0 {
        return Ok(());
    }
    let lift = RIB_UNDER - frame.cut.thickness;
    let surface = |k: usize, dy: f64| {
        let phi = TAU * fraction(k, HEM_AROUND);
        frame.on(frame.hem_at(phi) + height / 2.0 + dy, phi, lift)
    };
    band(
        mesh,
        &Band {
            around: HEM_AROUND,
            surface: &surface,
            height,
            thickness: frame.cut.thickness + RIB_PROUD - RIB_UNDER,
            colour,
            ribs: HEM_RIBS,
        },
    )
}

/// A hood's drawstrings: two cords hanging from the neckline either side of the centre front, each ending in an aglet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Drawstrings {
    /// Whether there are any.
    pub on: bool,
    /// How far they hang below the neckline, metres.
    pub length: f64,
    /// How far either side of the centre front they come out, radians.
    pub spread: f64,
    /// The cord's radius, metres.
    pub radius: f64,
    /// The cord's colour.
    pub colour: omuzaki_face_api::ColourRef,
    /// The aglets' colour.
    pub aglet: omuzaki_face_api::ColourRef,
}

impl Default for Drawstrings {
    fn default() -> Self {
        Self {
            on: true,
            length: 0.11,
            spread: 0.16,
            radius: 0.0016,
            colour: omuzaki_face_api::ColourRef::rgb(0xee, 0xec, 0xe6),
            aglet: omuzaki_face_api::ColourRef::rgb(0xc8, 0xc4, 0xbc),
        }
    }
}

impl Drawstrings {
    /// Checks the drawstrings, their fields under `drawstrings/`.
    pub fn check(&self, c: &mut omuzaki_face_api::Check) {
        c.range("drawstrings/length", self.length, 0.02, 0.3);
        c.range("drawstrings/spread", self.spread, 0.05, 0.6);
        c.range("drawstrings/radius", self.radius, 0.0005, 0.004);
    }
}

/// How long an aglet is, metres, and how much thicker than its cord.
const AGLET: (f64, f64) = (0.012, 1.35);
/// How far under the neckline the drawstrings come out, metres.
const DRAWSTRING_BELOW: f64 = 0.006;

/// Adds `strings` to `mesh` (cords in `colours.0`, aglets in `colours.1`, linear RGB): each down the outside from just
/// under the neckline, lying on the cloth, the hem's rib or the hem where it ends sooner.
///
/// # Errors
/// The surface's or the kit's.
pub fn drawstrings(
    mesh: &mut MeshBuilder,
    frame: &Frame,
    strings: &Drawstrings,
    colours: ([f32; 3], [f32; 3]),
    lowest: f64,
) -> Result<(), GarmentError> {
    const STEPS: usize = 24;
    if !strings.on {
        return Ok(());
    }
    let (aglet, thicker) = AGLET;
    for phi in [strings.spread, TAU - strings.spread] {
        let from = frame.neck(phi) - DRAWSTRING_BELOW;
        let to = (from - strings.length).max(lowest);
        for (span, radius, colour) in
            [((from, to), strings.radius, colours.0), ((to + aglet, to), strings.radius * thicker, colours.1)]
        {
            let (mut points, mut skins) = (Vec::with_capacity(STEPS + 1), Vec::with_capacity(STEPS + 1));
            for k in 0..=STEPS {
                let lift = frame.cut.lip + STANDOFF + radius;
                let (p, _, skin) = frame.on(lerp(span.0, span.1, fraction(k, STEPS)), phi, lift)?;
                points.push(p);
                skins.push(skin);
            }
            cord(mesh, &Tube { points: &points, skins: &skins, radius, colour })?;
        }
    }
    Ok(())
}

/// How far `p` stands outside the body under the coat (the column at no offset), metres, in its height's plane.
fn outside(frame: &Frame, p: DVec3) -> f64 {
    let Ok(ring) = frame.column.ring(p.y, 0.0) else { return f64::INFINITY };
    let d = p - ring.centre;
    ring.along_ray(omuzaki_core::glam::DVec2::new(d.dot(ring.across), d.dot(ring.through))).1
}
