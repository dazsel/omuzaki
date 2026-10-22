//! Rolled edges, cords and patches: the pieces a coat adds on its surface.
//!
//! - [`loop_tube`]: a round tube along a closed path (a hem, a neckline, the border of an open front), its rings square
//!   to the path and turned to face away from the body's axis, so they never twist round a corner.
//! - [`cord`]: a round tube along an open path with closed ends (a drawstring, a strap).
//! - [`patch`]: a thin slab lying on the surface (a lapel, a pocket, a collar band, a pouch), its top sheet and its
//!   underside joined by a rolled edge.

use omuzaki_core::glam::DVec3;

use super::shell::Skinning;
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::sweep::tube;
use crate::outfit::common::GarmentError;

/// Segments round a rolled edge.
pub const TUBE_SEGMENTS: usize = 8;
/// Points closer than this along a path are one.
const SAME: f64 = 1e-5;

/// A tube along a path.
pub struct Tube<'a> {
    /// The path's points.
    pub points: &'a [DVec3],
    /// Each point's skin.
    pub skins: &'a [Skinning],
    /// The tube's radius, metres.
    pub radius: f64,
    /// Its vertex colour (linear RGB).
    pub colour: [f32; 3],
}

/// The path without repeated points (and, for a loop, without an end that repeats its start), with their skins.
fn clean(points: &[DVec3], skins: &[Skinning], closed: bool) -> (Vec<DVec3>, Vec<Skinning>) {
    let (mut p, mut s): (Vec<DVec3>, Vec<Skinning>) = (Vec::new(), Vec::new());
    for (point, skin) in points.iter().zip(skins) {
        if p.last().is_none_or(|last: &DVec3| last.distance(*point) > SAME) {
            p.push(*point);
            s.push(skin.clone());
        }
    }
    while closed && p.len() > 1 && p[0].distance(p[p.len() - 1]) <= SAME {
        p.pop();
        s.pop();
    }
    (p, s)
}

/// A closed round tube along `tube.points` (the loop closes back to the first), each ring's first direction the one
/// away from `axis(p)`, a point on the body's axis at the path's height.
///
/// # Errors
/// [`GarmentError::Invalid`] for a loop of fewer than 3 distinct points; the kit's.
pub fn loop_tube(
    builder: &mut MeshBuilder,
    tube: &Tube<'_>,
    axis: &dyn Fn(DVec3) -> DVec3,
) -> Result<(), GarmentError> {
    let (centres, skins) = clean(tube.points, tube.skins, true);
    let n = centres.len();
    if n < 3 {
        return Err(GarmentError::invalid("edge", format!("a rolled edge needs 3 points, not {n}")));
    }
    let frame = |k: usize| -> (DVec3, DVec3) {
        let (p, a, b) = (centres[k % n], centres[(k + n - 1) % n], centres[(k + 1) % n]);
        let tangent = (b - a).normalize_or_zero();
        let away = p - axis(p);
        let radial = (away - tangent * away.dot(tangent)).try_normalize().unwrap_or(DVec3::Y);
        (radial, tangent.cross(radial).normalize_or_zero())
    };
    let frames: Vec<(DVec3, DVec3)> = (0..=n).map(frame).collect();
    let seg = TUBE_SEGMENTS;
    let grid = Grid::from_fn(n + 1, seg + 1, |i, j| {
        let a = TAU * fraction(j % seg, seg);
        let (normal, binormal) = frames[i];
        centres[i % n] + (normal * a.cos() + binormal * a.sin()) * tube.radius
    })?;
    let inside = |i: usize, _: usize| centres[i % n];
    let skin = |_: DVec3, i: usize, _: usize| skins[i % n].clone();
    let colour = |_: usize, _: usize, _: DVec3| [tube.colour[0], tube.colour[1], tube.colour[2], 1.0];
    let options = GridOptions {
        wrap_i: true,
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&colour),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)?;
    Ok(())
}

/// A round tube along the open path `tube.points` with both ends closed by a fan, so it adds no open border.
///
/// # Errors
/// [`GarmentError::Invalid`] for fewer than 2 distinct points; the kit's.
pub fn cord(builder: &mut MeshBuilder, tube: &Tube<'_>) -> Result<(), GarmentError> {
    let (points, skins) = clean(tube.points, tube.skins, false);
    if points.len() < 2 {
        return Err(GarmentError::invalid("cord", "a cord needs two distinct points"));
    }
    let swept = self::tube(&points, |_| tube.radius, TUBE_SEGMENTS)?;
    let centres = &swept.centres;
    let inside = |i: usize, _: usize| centres[i];
    let skin = |_: DVec3, i: usize, _: usize| skins[i].clone();
    let rgba = [tube.colour[0], tube.colour[1], tube.colour[2], 1.0];
    let colour = |_: usize, _: usize, _: DVec3| rgba;
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&colour),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    builder.add_grid(&swept.grid, &options)?;
    let last = points.len() - 1;
    for (row, outward) in [(0, points[0] - points[1]), (last, points[last] - points[last - 1])] {
        let mut ring: Vec<DVec3> = (0..TUBE_SEGMENTS).map(|j| swept.grid.at(row, j)).collect();
        let facing = ring.iter().enumerate().map(|(k, p)| p.cross(ring[(k + 1) % ring.len()])).sum::<DVec3>();
        if facing.dot(outward) < 0.0 {
            ring.reverse();
        }
        builder.add_polygon(&ring, outward.try_normalize().unwrap_or(DVec3::Y), rgba, &skins[row])?;
    }
    Ok(())
}

/// A patch: `rows x cols` points of its underside (row by row, lying on the garment), the outward normal at each, its
/// thickness and skins. Its top is the underside pushed out by the thickness.
pub struct Patch {
    /// Rows.
    pub rows: usize,
    /// Columns.
    pub cols: usize,
    /// The underside.
    pub points: Vec<DVec3>,
    /// The outward normals.
    pub normals: Vec<DVec3>,
    /// Each point's skin.
    pub skins: Vec<Skinning>,
    /// How thick it is, metres.
    pub thickness: f64,
    /// The top's colour and the underside's (linear RGB).
    pub colours: ([f32; 3], [f32; 3]),
    /// The rolled edge's colour.
    pub edge: [f32; 3],
}

/// Adds `patch`: its top, its underside and the rolled edge round its border. Two open borders (the top's and the
/// underside's), hidden under the roll.
///
/// # Errors
/// [`GarmentError::Invalid`] for a patch smaller than 2 x 2; the kit's.
pub fn patch(builder: &mut MeshBuilder, patch: &Patch) -> Result<(), GarmentError> {
    let (rows, cols) = (patch.rows, patch.cols);
    if rows < 2 || cols < 2 || patch.points.len() != rows * cols {
        return Err(GarmentError::invalid(
            "patch",
            format!("a {rows} x {cols} patch with {} points", patch.points.len()),
        ));
    }
    let top: Vec<DVec3> = patch.points.iter().zip(&patch.normals).map(|(p, n)| *p + *n * patch.thickness).collect();
    for (points, under) in [(top.clone(), false), (patch.points.clone(), true)] {
        let grid = Grid::new(rows, cols, points)?;
        let normals = &patch.normals;
        let inside = |i: usize, j: usize| patch.points[i * cols + j] - normals[i * cols + j];
        let skin = |_: DVec3, i: usize, j: usize| patch.skins[i * cols + j].clone();
        let tone = if under { patch.colours.1 } else { patch.colours.0 };
        let colour = |_: usize, _: usize, _: DVec3| [tone[0], tone[1], tone[2], 1.0];
        let options = GridOptions {
            invert: under,
            inside: Some(&inside),
            colour: Some(&colour),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        };
        builder.add_grid(&grid, &options)?;
    }
    // The border, half way up the patch, round from the first row's start.
    let at = |i: usize, j: usize| {
        let k = i * cols + j;
        (patch.points[k].midpoint(top[k]), patch.skins[k].clone(), patch.normals[k])
    };
    let mut border = Vec::new();
    border.extend((0..cols).map(|j| at(0, j)));
    border.extend((1..rows).map(|i| at(i, cols - 1)));
    border.extend((0..cols - 1).rev().map(|j| at(rows - 1, j)));
    border.extend((1..rows - 1).rev().map(|i| at(i, 0)));
    let points: Vec<DVec3> = border.iter().map(|b| b.0).collect();
    let skins: Vec<Skinning> = border.iter().map(|b| b.1.clone()).collect();
    let normals: Vec<DVec3> = border.iter().map(|b| b.2).collect();
    // The roll's rings face out of the patch: away from its middle, along the surface.
    #[allow(clippy::cast_precision_loss)] // a patch's point count is tiny
    let middle = patch.points.iter().sum::<DVec3>() / patch.points.len() as f64;
    let axis = |p: DVec3| {
        let k = points.iter().position(|q| q.distance(p) < SAME).unwrap_or(0);
        let n = normals[k];
        // A point inward of `p` along the surface: the patch's middle with the normal's part taken out.
        let d = middle - p;
        p + (d - n * d.dot(n))
    };
    let tube = Tube { points: &points, skins: &skins, radius: patch.thickness / 2.0 + 0.0004, colour: patch.edge };
    loop_tube(builder, &tube, &axis)
}

/// A band round the body: `around` points round it, `surface(k, dy)` the surface point (with its outward normal and
/// skin) of the `k`-th, `dy` metres above the band's middle line; and the band's height and thickness. Its section is a
/// rounded rectangle lying on the surface, swept round the body, so it follows the surface above and below its middle
/// and is closed all round: no open border.
pub struct Band<'a> {
    /// Points round the band.
    pub around: usize,
    /// The surface under it.
    pub surface: &'a dyn Fn(usize, f64) -> Result<(DVec3, DVec3, Skinning), GarmentError>,
    /// Its height, metres.
    pub height: f64,
    /// Its thickness, metres.
    pub thickness: f64,
    /// Its vertex colour (linear RGB).
    pub colour: [f32; 3],
}

/// Points round a band's section.
const BAND_SECTION: usize = 16;
/// The squareness of a band's section.
const BAND_SQUARE: f64 = 6.0;

/// Adds `band`.
///
/// # Errors
/// [`GarmentError::Invalid`] for a band of fewer than 3 points round; the surface's; the kit's.
#[allow(clippy::many_single_char_names)] // the grid's textbook names
pub fn band(builder: &mut MeshBuilder, band: &Band<'_>) -> Result<(), GarmentError> {
    let n = band.around;
    if n < 3 {
        return Err(GarmentError::invalid("band", format!("a band needs 3 points round, not {n}")));
    }
    let (half_t, half_h) = (band.thickness / 2.0, band.height / 2.0);
    let mut points = Vec::with_capacity((n + 1) * (BAND_SECTION + 1));
    let mut middles = Vec::with_capacity(n);
    let mut skins = Vec::with_capacity(n);
    for i in 0..n {
        let (p, normal, skin) = (band.surface)(i, 0.0)?;
        middles.push(p + normal * half_t);
        skins.push(skin);
    }
    for i in 0..=n {
        for j in 0..=BAND_SECTION {
            let a = TAU * fraction(j % BAND_SECTION, BAND_SECTION);
            let (s, c) = (crate::kit::curve::sup(a.sin(), BAND_SQUARE), crate::kit::curve::sup(a.cos(), BAND_SQUARE));
            let (p, normal, _) = (band.surface)(i % n, s * half_h)?;
            points.push(p + normal * (half_t + c * half_t));
        }
    }
    let grid = Grid::new(n + 1, BAND_SECTION + 1, points)?;
    let inside = |i: usize, _: usize| middles[i % n];
    let skin = |_: DVec3, i: usize, _: usize| skins[i % n].clone();
    let colour = |_: usize, _: usize, _: DVec3| [band.colour[0], band.colour[1], band.colour[2], 1.0];
    let options = GridOptions {
        wrap_i: true,
        wrap_j: true,
        inside: Some(&inside),
        colour: Some(&colour),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    builder.add_grid(&grid, &options)?;
    Ok(())
}
