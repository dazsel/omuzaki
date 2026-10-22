//! The skirt's hem: the rolled bead along the hem line and the two thin band lines just above it.
//!
//! The port of the rolled hem and the band lines of `buildSkirt` (`uniform.js` 357-375) and `ringTube` (`uniform.js`
//! 44-56). The roll is a round bead (a closed ring of rings) half the cloth's thickness inside the outer skin and a hair
//! above the hem, and the band lines are 4-sided tubes that follow the pleats' folds.

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{DVec2, DVec3};

use super::pleats::{Column, Plan, Skins, THICKNESS};
use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};

/// How much higher than the outer hem the roll's centre is, metres.
const ROLL_LIFT: f64 = 0.0004;
/// The roll's segments round its section (`SEGC`).
pub const ROLL_SEGMENTS: usize = 8;
/// The roll's texture coordinate `v`: a row of the plaid beyond the skirt's own.
pub const ROLL_V: f64 = 2.4;
/// The roll's colour.
pub const ROLL_TONE: [f32; 3] = [0.95, 1.0, 0.97];
/// The heights of the band lines above the hem, metres, and how far they stand off the cloth.
pub const BAND_HEIGHTS: [f64; 2] = [0.0143, 0.0097];
/// See [`BAND_HEIGHTS`].
const BAND_OUT: f64 = 0.0004;
/// A band line's radius, metres, and its sides.
pub const BAND_RADIUS: f64 = 0.00042;
/// See [`BAND_RADIUS`].
pub const BAND_SIDES: usize = 4;
/// A tube section's tangent is taken as vertical when its radial direction is parallel to the path to this squared
/// length.
const DEGENERATE: f64 = 1e-8;

/// A closed tube with the centres of its rings (the inside its normals point away from).
pub struct Tube {
    /// The tube's rings: one per path point, and a closing one.
    pub grid: Grid,
    /// The path point of every ring.
    pub centres: Vec<DVec3>,
}

/// The roll round the hem of `skins`: a ring of `ROLL_SEGMENTS` segments of `radius` at every column but the closing
/// one, and the closing ring. Its centres are the bead's path (also the tube's inside reference).
///
/// # Errors
/// [`KitError`] for a grid the kit refuses.
pub fn roll(skins: &Skins, radius: f64) -> Result<Tube, KitError> {
    let outer = &skins.outer;
    let (last, columns) = (outer.rows() - 1, outer.cols() - 1);
    // The centre of the bead and the direction it bulges: into the cloth, along the horizontal normal.
    let beads: Vec<(DVec3, DVec3)> = (0..columns)
        .map(|j| {
            let (o, n) = (outer.at(last, j), skins.normals.normals[last * outer.cols() + j]);
            let k = n.x.hypot(n.z);
            let k = if k == 0.0 { 1.0 } else { k };
            let nh = DVec3::new(n.x / k, 0.0, n.z / k);
            (DVec3::new(o.x - nh.x * THICKNESS / 2.0, o.y + ROLL_LIFT, o.z - nh.z * THICKNESS / 2.0), nh)
        })
        .collect();
    let grid = Grid::from_fn(columns + 1, ROLL_SEGMENTS + 1, |i, q| {
        let (p, nh) = beads[i % columns];
        // The closing column repeats the first exactly, so the seam welds.
        let a = if q == ROLL_SEGMENTS { 0.0 } else { TAU * fraction(q, ROLL_SEGMENTS) };
        DVec3::new(p.x + nh.x * a.cos() * radius, p.y + a.sin() * radius, p.z + nh.z * a.cos() * radius)
    })?;
    let centres: Vec<DVec3> = (0..=columns).map(|i| beads[i % columns].0).collect();
    Ok(Tube { grid, centres })
}

/// The band line `height` metres above each column's hem: its path round the skirt.
pub fn band_path(plan: &Plan, columns: &[Column], height: f64) -> Vec<DVec3> {
    columns[..columns.len() - 1]
        .iter()
        .map(|c| {
            let y = plan.hem_of(c) + height;
            let b = plan.base(y, c.phi);
            let off = plan.amplitude(y) * c.fold + BAND_OUT;
            DVec3::new(b.x + b.nx * off, y, b.z + b.nz * off)
        })
        .collect()
}

/// A closed tube of `radius` and `sides` round the closed loop `path`, twist-free: each section's frame points from the
/// axis (through x = 0, z = 0) outward with the path's direction taken out (`ringTube`).
///
/// # Errors
/// [`KitError`] for a grid the kit refuses.
pub fn ring_tube(path: &[DVec3], radius: f64, sides: usize) -> Result<Tube, KitError> {
    let n = path.len();
    let grid = Grid::from_fn(n + 1, sides + 1, |k, j| {
        let (p, before, after) = (path[k % n], path[(k + n - 1) % n], path[(k + 1) % n]);
        let t = (after - before).normalize_or_zero();
        let radial = DVec3::new(p.x, 0.0, p.z).normalize_or_zero();
        let mut normal = radial - t * radial.dot(t);
        if normal.length_squared() < DEGENERATE {
            normal = DVec3::Y;
        }
        let normal = normal.normalize_or_zero();
        let binormal = t.cross(normal).normalize_or_zero();
        let a = if j == sides { 0.0 } else { TAU * fraction(j, sides) };
        p + normal * (a.cos() * radius) + binormal * (a.sin() * radius)
    })?;
    let centres = (0..=n).map(|k| path[k % n]).collect();
    Ok(Tube { grid, centres })
}

/// How a vertex of a hem part is skinned: from its position.
pub type Weights<'a> = &'a dyn Fn(DVec3, usize, usize) -> Vec<(JointRef, f32)>;

/// Adds the roll of `radius` round the hem of `skins` to `builder`, textured by `(arc lengths, tile)` and coloured by
/// `colour`.
///
/// # Errors
/// [`KitError`] as [`roll`] and [`MeshBuilder::add_grid`].
pub fn add_roll(
    builder: &mut MeshBuilder,
    skins: &Skins,
    radius: f64,
    (arcs, tile): (&[f64], f64),
    colour: &dyn Fn(usize, usize, DVec3) -> [f32; 4],
    weights: Weights<'_>,
) -> Result<(), KitError> {
    let tube = roll(skins, radius)?;
    let beads = skins.outer.cols() - 1;
    let inside = |i: usize, _: usize| tube.centres[i];
    // The roll's texture row is below the cloth's own, counted downward like the cloth's `v` (glTF's `v` runs down).
    let uv = |i: usize, _: usize| DVec2::new(arcs[i % beads] / tile, 1.0 - ROLL_V);
    builder.add_grid(
        &tube.grid,
        &GridOptions {
            wrap_i: true,
            wrap_j: true,
            inside: Some(&inside),
            uv: Some(&uv),
            colour: Some(colour),
            skin: Skin::Per(weights),
            ..GridOptions::default()
        },
    )
}

/// Adds the two band lines above the hem to `builder`.
///
/// # Errors
/// [`KitError`] as [`ring_tube`] and [`MeshBuilder::add_grid`].
pub fn add_bands(
    builder: &mut MeshBuilder,
    plan: &Plan,
    columns: &[Column],
    weights: Weights<'_>,
) -> Result<(), KitError> {
    for height in BAND_HEIGHTS {
        let tube = ring_tube(&band_path(plan, columns, height), BAND_RADIUS, BAND_SIDES)?;
        let inside = |i: usize, _: usize| tube.centres[i];
        builder.add_grid(
            &tube.grid,
            &GridOptions {
                wrap_i: true,
                wrap_j: true,
                inside: Some(&inside),
                skin: Skin::Per(weights),
                ..GridOptions::default()
            },
        )?;
    }
    Ok(())
}
