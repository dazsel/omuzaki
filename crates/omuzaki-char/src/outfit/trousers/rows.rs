//! The trousers' mesh as rows of vertices: the pelvis from the waist down to the crotch, then each leg from the crotch
//! to the hem, with the leg's first rows morphing from the pelvis ring's half to the leg's own ring.
//!
//! **Why the pelvis is its own section.** The body's thighs are almost touching where they leave the torso, and cloth
//! cannot make two walls between them. So the trousers are one tube round both thighs from the waist down to where the
//! legs part ([`crotch`]); the tube's section goes from the torso's own (at the bottom of the torso region) to a
//! rounded rectangle round the two thighs ([`PairRing`]). Where the legs part, the ring splits in two by a chord down
//! the middle, and each half morphs into its leg's ring over a short gusset.

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{DVec2, DVec3};

use super::shape::MID_GAP;
use crate::kit::curve::{TAU, fraction, lerp, sstep, sup};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::skin::{MAX_INFLUENCES, limit_influences, normalise};
use crate::outfit::common::GarmentError;
use crate::outfit::loft::{Ring, Surface};

/// Segments round the pelvis: even, so the front and the back of the body are columns, and half of them make each leg's
/// outer arc.
pub const PELVIS_SEGMENTS: usize = 48;
/// Intervals along the chord between the legs.
pub const CHORD_INTERVALS: usize = 4;
/// Rows of the gusset, from the pelvis ring's half to the leg's ring.
pub const GUSSET_ROWS: usize = 4;
/// How far down the leg the gusset reaches, as a fraction of the leg region.
pub const GUSSET: f64 = 0.04;
/// The least the legs part below where they leave the torso, as a fraction of the leg region (about 3 cm on soft-sd): the
/// rows from the torso to the crotch need room to turn from the torso's section into the thighs'.
pub const MIN_FALL: f64 = 0.08;
/// Rows from the bottom of the torso region to the crotch.
pub const LOWER_ROWS: usize = 8;
/// The exponent of the rounded rectangle round both thighs.
pub const PAIR_EXPONENT: f64 = 4.0;
/// How far outside the two leg rings the rectangle stands, metres: a rounded rectangle cannot hug two ellipses, and the
/// legs' own ease is only just enough at its corners.
pub const PAIR_MARGIN: f64 = 0.0015;
/// How far down from the bottom of the torso region the pelvis tube is still the torso's cylinder, as a fraction of the
/// way to the crotch, before it turns into the thighs' ring.
pub const CYLINDER_FRACTION: f64 = 0.7;
/// Where the thighs' share of the skin starts, as a fraction of the torso region above its bottom.
pub const THIGH_SKIN_FROM: f64 = 0.30;
/// The half-width of the hand-over between the two thighs' skin at the middle of the body, as a fraction of the
/// distance from the middle to a thigh's centre.
pub const THIGH_SPLIT: f64 = 1.0;

/// The skin of a vertex.
pub type Skinning = Vec<(JointRef, f32)>;

/// A ring of vertices of the lofted mesh.
pub struct Row {
    /// The vertices round the ring (the last repeats the first).
    pub points: Vec<DVec3>,
    /// A point inside the garment here, which the winding is oriented away from.
    pub inside: DVec3,
    /// The skin of each vertex.
    pub skins: Vec<Skinning>,
}

/// `a` blended toward `b` by `s`, per joint, as at most [`MAX_INFLUENCES`] normalised weights.
pub fn mix_skin(a: &[(JointRef, f32)], b: &[(JointRef, f32)], s: f32) -> Skinning {
    let mut out: Skinning = a.iter().map(|(j, w)| (j.clone(), w * (1.0 - s))).collect();
    for (joint, w) in b {
        match out.iter_mut().find(|(j, _)| j == joint) {
            Some(entry) => entry.1 += w * s,
            None => out.push((joint.clone(), w * s)),
        }
    }
    out.retain(|(_, w)| *w > 0.0);
    limit_influences(&mut out, MAX_INFLUENCES);
    normalise(&mut out);
    out
}

/// Adds `rows` (all the same length, the last vertex of each repeating the first) to `builder` as one grid.
///
/// # Errors
/// [`GarmentError::Invalid`] for rows of different lengths; the kit's errors.
pub fn add_rows(builder: &mut MeshBuilder, rows: &[Row], uvs: Option<&[DVec2]>) -> Result<(), GarmentError> {
    let cols = rows.first().map_or(0, |r| r.points.len());
    if rows.iter().any(|r| r.points.len() != cols) {
        return Err(GarmentError::invalid("rows", "the rows of a grid must be the same length"));
    }
    let grid = Grid::new(rows.len(), cols, rows.iter().flat_map(|r| r.points.iter().copied()).collect())?;
    let inside = |i: usize, _: usize| rows[i].inside;
    let skin = |_: DVec3, i: usize, j: usize| rows[i].skins[j].clone();
    let uv = |i: usize, j: usize| uvs.map_or(DVec2::ZERO, |u| u[i * cols + j]);
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        uv: uvs.map(|_| &uv as &dyn Fn(usize, usize) -> DVec2),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)?;
    Ok(())
}

/// UVs that count fabric tiles of `tile` metres along each row and down each column, every row rounded to whole tiles
/// round so the seam matches.
pub fn tile_uvs(rows: &[Row], tile: f64) -> Vec<DVec2> {
    let cols = rows.first().map_or(0, |r| r.points.len());
    let mut out = vec![DVec2::ZERO; rows.len() * cols];
    let mut v = vec![0.0; cols];
    for (i, row) in rows.iter().enumerate() {
        let mut along = vec![0.0; cols];
        for j in 1..cols {
            along[j] = along[j - 1] + row.points[j].distance(row.points[j - 1]);
        }
        let width = along[cols - 1];
        let scale = if width > 0.0 { (width / tile).round().max(1.0) / width } else { 0.0 };
        for j in 0..cols {
            if i > 0 {
                v[j] += row.points[j].distance(rows[i - 1].points[j]) / tile;
            }
            out[i * cols + j] = DVec2::new(along[j] * scale, v[j]);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------- crotch

/// The inner edge of a leg's garment ring at `t`, metres from the middle of the body, positive when the ring has not
/// crossed it.
fn inner_edge(leg: &Surface, t: f64, side: f64) -> Result<f64, GarmentError> {
    let ring = leg.ring(t)?;
    let steps = 32;
    let edge = (0..steps).map(|k| side * ring.point(TAU * fraction(k, steps)).x).fold(f64::INFINITY, f64::min);
    Ok(edge)
}

/// Where along the legs they part: the first `t` from `from` down where both legs' garment rings are clear of the middle
/// by more than a quarter of the ease and [`MID_GAP`], so the chord between them keeps half the ease from each leg.
///
/// # Errors
/// [`GarmentError::Invalid`] if the legs never part; the surfaces' errors.
pub fn crotch(legs: &[Surface; 2], from: f64, ease: f64) -> Result<f64, GarmentError> {
    let need = 0.25 * ease + MID_GAP;
    let steps = 128;
    for k in 0..=steps {
        let t = lerp(from, 1.0, fraction(k, steps));
        if inner_edge(&legs[0], t, 1.0)? >= need && inner_edge(&legs[1], t, -1.0)? >= need {
            return Ok(t);
        }
    }
    Err(GarmentError::invalid("legs", "the legs never part far enough for cloth between them"))
}

// ------------------------------------------------------------------------------------------------------------ pelvis

/// The rounded rectangle round both thighs at one height.
pub struct PairRing {
    centre: DVec3,
    across: DVec3,
    through: DVec3,
    radii: DVec2,
}

impl PairRing {
    /// The ring round the two leg rings at `t` of the legs (each ring is its leg's body section with the ease) and
    /// round those at `until` further down, which the legs go on to: the legs are not always narrowest where they part.
    ///
    /// # Errors
    /// The surfaces' errors.
    pub fn at(legs: &[Surface; 2], t: f64, until: f64) -> Result<Self, GarmentError> {
        let (left, right) = (legs[0].ring(t)?, legs[1].ring(t)?);
        let centre = (left.centre + right.centre) / 2.0;
        let (across, through) = (left.across, left.through);
        let below = [legs[0].ring(until)?, legs[1].ring(until)?];
        let rings = [left, right, below[0], below[1]];
        let offset = |ring: &Ring| {
            let d = ring.centre - centre;
            DVec2::new(d.dot(across).abs(), d.dot(through).abs())
        };
        // Wide enough to reach the outside of the farthest leg, then deep enough that the rounded rectangle passes
        // over the front (and back) of each leg's ellipse where it is farthest from the middle: the rectangle dips
        // between the two thighs' fronts, so the depth of a leg is divided by how far it dips at that leg's centre.
        let width = rings.iter().map(|r| offset(r).x + r.radii.x).fold(0.0, f64::max) + PAIR_MARGIN;
        let depth = rings
            .iter()
            .map(|r| {
                let o = offset(r);
                let dip = (1.0 - (o.x / width).powf(PAIR_EXPONENT)).max(f64::EPSILON).powf(1.0 / PAIR_EXPONENT);
                (o.y + r.radii.y) / dip
            })
            .fold(0.0, f64::max)
            + PAIR_MARGIN;
        Ok(Self { centre, across, through, radii: DVec2::new(width, depth) })
    }

    /// `p` pushed out along the ray from the ring's centre to the ring's edge, if it is inside it: the point of the ring
    /// in the direction of `p`.
    pub fn enclose(&self, p: DVec3) -> DVec3 {
        let v = p - self.centre;
        let (x, z) = (v.dot(self.across), v.dot(self.through));
        let len = x.hypot(z);
        if len <= f64::EPSILON {
            return p;
        }
        let (ux, uz) = (x / len, z / len);
        let reach = ((ux.abs() / self.radii.x).powf(PAIR_EXPONENT) + (uz.abs() / self.radii.y).powf(PAIR_EXPONENT))
            .powf(-1.0 / PAIR_EXPONENT);
        if len >= reach {
            return p;
        }
        p + (self.across * ux + self.through * uz) * (reach - len)
    }

    /// The point at `phi`.
    pub fn point(&self, phi: f64) -> DVec3 {
        self.centre
            + self.across * (sup(phi.sin(), PAIR_EXPONENT) * self.radii.x)
            + self.through * (sup(phi.cos(), PAIR_EXPONENT) * self.radii.y)
    }
}

/// The angle of column `j` of the pelvis: a full turn, the last column exactly the first.
fn pelvis_phi(j: usize) -> f64 {
    if j == PELVIS_SEGMENTS { 0.0 } else { TAU * fraction(j, PELVIS_SEGMENTS) }
}

/// The share of each thigh's skin at a vertex at horizontal position `x`, and how much of the vertex is on the thighs at
/// all (`thighs`, 0 to 1): the thighs' bones take over from the hips and spine as the rows go down.
fn pelvis_skin(
    torso: &[(JointRef, f32)],
    thighs: [&JointRef; 2],
    x: f64,
    half_separation: f64,
    thighs_share: f64,
) -> Skinning {
    let left = sstep(-THIGH_SPLIT * half_separation, THIGH_SPLIT * half_separation, x);
    #[allow(clippy::cast_possible_truncation)] // weights are f32
    let split: Skinning = vec![(thighs[0].clone(), left as f32), (thighs[1].clone(), (1.0 - left) as f32)];
    #[allow(clippy::cast_possible_truncation)]
    mix_skin(torso, &split, thighs_share as f32)
}

/// The pelvis rows from the waist (`torso_ts`, top first, ending at the bottom of the torso region) down to the
/// crotch at `crotch_t` along the legs.
///
/// # Errors
/// The surfaces' errors.
pub fn pelvis_rows(
    torso: &Surface,
    legs: &[Surface; 2],
    torso_ts: &[f64],
    crotch_t: f64,
    shaped_torso: &dyn Fn(f64, f64) -> Result<DVec3, GarmentError>,
) -> Result<Vec<Row>, GarmentError> {
    let thighs = [&legs[0].weights().chain()[0].0, &legs[1].weights().chain()[0].0];
    let bottom = torso.ring(0.0)?;
    let top = torso.ring(1.0)?;
    let length = top.centre.distance(bottom.centre);
    let up = bottom.axis;
    let leg_top = legs[0].project(bottom.centre)?.map(|p| p.t).ok_or_else(|| {
        GarmentError::invalid("legs", "no section of the left leg passes through the bottom of the torso")
    })?;
    let separation = legs[0].ring(crotch_t)?.centre.x.abs();
    // How far down the torso the thighs start taking over the skin, and how far the crotch is.
    let skin_from = bottom.centre + up * (THIGH_SKIN_FROM * length);
    let until = crotch_t + GUSSET;
    let crotch_pair = PairRing::at(legs, crotch_t, until)?;
    let reach = (skin_from - crotch_pair.centre).dot(up).max(f64::EPSILON);
    let share = |height: DVec3| sstep(0.0, 1.0, (skin_from - height).dot(up) / reach);
    let mut rows = Vec::new();
    for &t in torso_ts {
        let ring = torso.ring(t)?;
        let mut points =
            (0..=PELVIS_SEGMENTS).map(|j| shaped_torso(t, pelvis_phi(j))).collect::<Result<Vec<_>, _>>()?;
        // Where the legs have started (they begin above the bottom of the torso region), the cloth must also clear them:
        // on a body whose hips are narrower than its thighs the legs poke out of the torso's section.
        if let Some(at) = legs[0].project(ring.centre)? {
            let pair = PairRing::at(legs, at.t, at.t)?;
            for p in &mut points {
                *p = pair.enclose(*p);
            }
        }
        let weights = torso.weights().at(t);
        let thighs_share = share(ring.centre);
        let skins = points.iter().map(|p| pelvis_skin(&weights, thighs, p.x, separation, thighs_share)).collect();
        rows.push(Row { points, inside: ring.centre, skins });
    }
    let cylinder: Vec<DVec3> = (0..=PELVIS_SEGMENTS).map(|j| bottom.point(pelvis_phi(j)) - bottom.centre).collect();
    let weights = torso.weights().at(0.0);
    // The rows go straight down from the bottom of the torso region to the crotch, however the legs lean: a row that
    // rose above that plane would be inside the torso.
    let step = crotch_pair.centre - bottom.centre;
    let fall = -step.dot(up);
    if fall <= 0.0 {
        return Err(GarmentError::invalid("legs", "the legs part above the bottom of the torso"));
    }
    let sideways = step - up * step.dot(up);
    for r in 1..=LOWER_ROWS {
        let u = fraction(r, LOWER_ROWS);
        let pair = PairRing::at(legs, lerp(leg_top, crotch_t, u), until)?;
        let centre = bottom.centre - up * (fall * u) + sideways * u;
        let blend = sstep(0.0, CYLINDER_FRACTION, u);
        let points: Vec<DVec3> = (0..=PELVIS_SEGMENTS)
            .map(|j| {
                let c = cylinder[j];
                let p = pair.point(pelvis_phi(j)) - pair.centre;
                centre + c + (p - c) * blend
            })
            .map(|q| pair.enclose(q))
            .collect();
        let thighs_share = if r == LOWER_ROWS { 1.0 } else { share(centre) };
        let skins = points.iter().map(|p| pelvis_skin(&weights, thighs, p.x, separation, thighs_share)).collect();
        rows.push(Row { points, inside: centre, skins });
    }
    Ok(rows)
}

// -------------------------------------------------------------------------------------------------------------- legs

/// The rows of one leg from the crotch down: the pelvis ring's half at `pelvis` (its last row), morphing over the gusset
/// into the leg's ring at `t_start`, then the leg's rings at `ts`.
///
/// `side` is `+1` for the leg on the +x side and `-1` for the other.
///
/// # Errors
/// [`GarmentError::Invalid`] if the leg's ring isn't inside the pelvis ring's half (the pair ring doesn't enclose it);
/// the surfaces' errors.
#[allow(clippy::too_many_arguments)] // the leg's whole description: its surface, side, the rows above and where it runs
pub fn leg_rows(
    leg: &Surface,
    side: f64,
    pelvis: &Row,
    crotch_t: f64,
    ts: &[f64],
    shaped: &dyn Fn(f64, f64) -> Result<DVec3, GarmentError>,
    thighs: &[JointRef; 2],
) -> Result<Vec<Row>, GarmentError> {
    let half = PELVIS_SEGMENTS / 2;
    // The half of the pelvis ring on this leg's side, front to back (the +x side) or back to front (the other side).
    let (arc, chord_forward) = if side > 0.0 { (0..=half, false) } else { (half..=PELVIS_SEGMENTS, true) };
    let (front, back) = (pelvis.points[0], pelvis.points[half]);
    let chord = |i: usize| back + (front - back) * fraction(i, CHORD_INTERVALS);
    // The chord is where the two legs' cloth meets; each leg's side of it follows its own thigh, so that when the legs part
    // the cloth between them opens along the chord (inside the crotch, out of sight) instead of tearing.
    let own = if side > 0.0 { &thighs[0] } else { &thighs[1] };
    let chord_skin: Skinning = vec![(own.clone(), 1.0)];
    let mut loop_points: Vec<DVec3> = arc.clone().map(|j| pelvis.points[j]).collect();
    let mut loop_skins: Vec<Skinning> = arc.map(|j| pelvis.skins[j].clone()).collect();
    let interior: Vec<usize> =
        if chord_forward { (1..CHORD_INTERVALS).rev().collect() } else { (1..CHORD_INTERVALS).collect() };
    for i in interior {
        loop_points.push(chord(i));
        loop_skins.push(chord_skin.clone());
    }
    // Each vertex of the loop, as an angle of the leg's ring.
    let ring = leg.ring(crotch_t)?;
    let phis: Vec<f64> = loop_points
        .iter()
        .map(|p| {
            let d = *p - ring.centre;
            ring.along_ray(DVec2::new(d.dot(ring.across), d.dot(ring.through))).0
        })
        .collect();
    let t_start = crotch_t + GUSSET;
    let start = leg.ring(t_start)?;
    let skin_start = leg.weights().at(t_start);
    let mut rows = Vec::new();
    let close = |mut v: Vec<DVec3>| {
        v.push(v[0]);
        v
    };
    let finish = |mut s: Vec<Skinning>| {
        s.push(s[0].clone());
        s
    };
    let targets: Vec<DVec3> = phis.iter().map(|&phi| shaped(t_start, phi)).collect::<Result<_, _>>()?;
    for (m, (v, c)) in loop_points.iter().zip(&targets).enumerate() {
        let (reach_v, reach_c) = ((*v - ring.centre).length(), (*c - start.centre).length());
        if reach_c > reach_v {
            return Err(GarmentError::invalid(
                "legs",
                format!(
                    "the leg's ring at column {m} of the gusset is {:.2} mm outside the thighs' ring: widen the pair",
                    (reach_c - reach_v) * 1e3
                ),
            ));
        }
    }
    for g in 0..=GUSSET_ROWS {
        let u = sstep(0.0, 1.0, fraction(g, GUSSET_ROWS));
        let points: Vec<DVec3> = loop_points.iter().zip(&targets).map(|(v, c)| *v + (*c - *v) * u).collect();
        #[allow(clippy::cast_possible_truncation)] // weights are f32
        let skins: Vec<Skinning> = loop_skins.iter().map(|s| mix_skin(s, &skin_start, u as f32)).collect();
        let inside = ring.centre + (start.centre - ring.centre) * u;
        rows.push(Row { points: close(points), inside, skins: finish(skins) });
    }
    for &t in ts {
        let r = leg.ring(t)?;
        let points = phis.iter().map(|&phi| shaped(t, phi)).collect::<Result<Vec<_>, _>>()?;
        let skin = leg.weights().at(t);
        let skins = vec![skin; points.len()];
        rows.push(Row { points: close(points), inside: r.centre, skins: finish(skins) });
    }
    Ok(rows)
}
