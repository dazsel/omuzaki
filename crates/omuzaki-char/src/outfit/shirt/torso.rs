//! The shirt's body: the loft from the hem to the neck opening, and the dark lining under the hem (`uniform.js`
//! `buildShirtTorso`, 58-76).
//!
//! The body is one grid of 37 rows by 61 columns (the last column repeats the first). Its rows are dense where the
//! shirt curves most: eight from the hem to the waist, ten to the chest, fourteen over the chest up to the shoulders and
//! four more up the neck to the opening. The prototype wrote them as absolute heights of its own body; here they are the
//! same fractions of the torso's length ([`along`]), so the shirt follows whatever torso it is given.
//!
//! The lining exists to be tucked: when [`TorsoSpec::tuck`] pulls the hips end of the shirt in (so the pleats of a skirt
//! hide it), the dark lining closes the shirt's bottom so nothing white shows under the skirt's hem. A shirt without a
//! tuck needs no lining.

use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{DVec2, DVec3};

use super::frame::Column;
use super::tile_uvs;
use crate::kit::curve::{TAU, fraction, rows_of, sstep};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::outfit::common::GarmentError;
use crate::outfit::loft::{RegionWeights, Ring};

/// Segments round the shirt: columns are one more, the last repeating the first.
pub const SEGMENTS: usize = 60;
/// Rows from the hem to the waist, from the waist to the chest, over the chest to the neck base, and up the neck.
pub const ROW_COUNTS: [usize; 4] = [8, 10, 14, 4];
/// Rows of the lining above its closed bottom.
pub const LINING_ROWS: usize = 6;
/// The bottom of the lining closes in three rings at the shirt's lowest height, each this much of the shirt's radius.
pub const LINING_POLE: [f64; 3] = [0.0, 0.45, 0.8];
/// How far the lining sits inside the shirt, metres: under it, never through it.
pub const LINING_INSET: f64 = 0.0003;
/// How far the lining reaches up past the hem, metres, so no gap shows between them.
pub const LINING_OVERLAP: f64 = 0.008;

/// The soft-sd torso the prototype's heights were written for: from `y = 0.325` (hips) to `0.545` (neck base).
const REF_BOTTOM: f64 = 0.325;
const REF_TOP: f64 = 0.545;
/// The prototype's heights on that torso, metres: the vest hem line, the waist, the chest, and the hips end of the
/// tuck's smoothstep (`0.385` where it is 0, `0.34` where it is full).
const REF_HEM: f64 = 0.384;
const REF_WAIST: f64 = 0.46;
const REF_CHEST: f64 = 0.50;
const REF_TUCK_NONE: f64 = 0.385;
const REF_TUCK_FULL: f64 = 0.34;

/// The fraction of the torso's length at which the prototype's height `y` (on the soft-sd torso) sits.
pub const fn along(y: f64) -> f64 {
    (y - REF_BOTTOM) / (REF_TOP - REF_BOTTOM)
}

/// The prototype's height on the soft-sd torso at the fraction `f` of a torso's length: the inverse of [`along`].
pub const fn to_reference(f: f64) -> f64 {
    REF_BOTTOM + f * (REF_TOP - REF_BOTTOM)
}

/// Where the hem sits by default, as a fraction of the torso's length: the prototype's vest hem line.
pub const DEFAULT_HEM: f64 = along(REF_HEM);

/// What the shirt's body is cut from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TorsoSpec {
    /// How far the shirt stands off the fit, metres.
    pub ease: f64,
    /// Where the hem sits, as a fraction of the torso's length.
    pub hem: f64,
    /// How far the neck opening stands above the torso's top, metres.
    pub rise: f64,
    /// How far the hips end is pulled in, as a fraction of its radius.
    pub tuck: f64,
    /// The front of the neck opening cut down, or `None` for an opening that follows the neck all round.
    pub cut: Option<Cut>,
}

/// A neckline cut into the front of the opening: a V or a scoop.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Cut {
    /// How far below the top of the torso the cut reaches at the front centre, metres.
    pub depth: f64,
    /// How far round the neck the cut spans, as a share of the whole way round.
    pub width: f64,
    /// A V (straight sides to a point) rather than a scoop (a rounded curve).
    pub v: bool,
}

impl Cut {
    /// How much of the cut's depth the edge drops at angle `phi` (0 is the front centre): 1 there, 0 at the cut's
    /// sides and beyond.
    fn profile(&self, phi: f64) -> f64 {
        let half = (self.width * TAU / 2.0).max(1e-6);
        // The angle's distance from the front (phi = 0), folded into 0..pi.
        let from_front = TAU / 2.0 - (phi.rem_euclid(TAU) - TAU / 2.0).abs();
        let u = (from_front / half).clamp(0.0, 1.0);
        if self.v { 1.0 - u } else { (1.0 - u * u).sqrt() }
    }
}

/// The body of the shirt.
#[derive(Debug)]
pub struct Torso {
    /// The shirt: the hem to the neck opening.
    pub shirt: MeshBuilder,
    /// The lining under the hem; only a tucked shirt has one.
    pub lining: Option<MeshBuilder>,
    /// The ring of the neck opening: where the collar stands.
    pub opening: Ring,
}

/// One row of a grid: the ring it sits on, how much of the radius it keeps, and its skin.
struct Row {
    ring: Ring,
    keep: f64,
    skin: Vec<(JointRef, f32)>,
}

/// The skin of a row at height `y`: the torso's weights up to its top, the neck bone above it.
fn skin_at(column: &Column, weights: &RegionWeights, y: f64) -> Vec<(JointRef, f32)> {
    if y > column.top() {
        return vec![(JointRef::NECK, 1.0)];
    }
    weights.at(((y - column.bottom()) / column.length()).clamp(0.0, 1.0))
}

/// The pull-in at height `y`: nothing from the prototype's `0.385` up, all of `tuck` from `0.34` down.
fn tuck_at(column: &Column, tuck: f64, y: f64) -> f64 {
    let at = |reference: f64| column.bottom() + along(reference) * column.length();
    tuck * sstep(at(REF_TUCK_NONE), at(REF_TUCK_FULL), y)
}

/// The grid of `rows` round the column, `SEGMENTS` segments, each row's points scaled about its centre by its `keep`.
fn grid_of(rows: &[Row]) -> Result<Grid, GarmentError> {
    Ok(Grid::from_fn(rows.len(), SEGMENTS + 1, |i, j| {
        // The last column repeats the first exactly, so the seam welds.
        let phi = if j == SEGMENTS { 0.0 } else { TAU * fraction(j, SEGMENTS) };
        let row = &rows[i];
        let centre = row.ring.centre;
        centre + (row.ring.point(phi) - centre) * row.keep
    })?)
}

/// Adds `grid` round the column to a fresh builder.
fn builder_of(rows: &[Row], tile: Option<f64>) -> Result<MeshBuilder, GarmentError> {
    let grid = grid_of(rows)?;
    let uvs = tile_uvs(&grid, tile, true);
    let uv = |i: usize, j: usize| uvs.as_ref().map_or(DVec2::ZERO, |u| u[i * grid.cols() + j]);
    let inside = |i: usize, _: usize| rows[i].ring.centre;
    let skin = |_: DVec3, i: usize, _: usize| rows[i].skin.clone();
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        uv: uvs.is_some().then_some(&uv as &dyn Fn(usize, usize) -> DVec2),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut builder = MeshBuilder::with_colours();
    builder.add_grid(&grid, &options)?;
    Ok(builder)
}

/// The shirt's body with its neckline cut: each column's rows are drawn down so that column's top lands on the cut's
/// edge, from a height a little under the cut's bottom (below it the shirt is as it would be uncut). The rows stay
/// in order, so the grid keeps its shape and its seams.
fn cut_builder(
    column: &Column,
    weights: &RegionWeights,
    spec: &TorsoSpec,
    cut: &Cut,
    ys: &[f64],
    tile: Option<f64>,
) -> Result<MeshBuilder, GarmentError> {
    let (hem, top) = (ys.first().copied().unwrap_or_default(), ys.last().copied().unwrap_or_default());
    let depth = cut.depth.min(top - hem - 0.02).max(0.0);
    let floor = (top - spec.rise - depth - 0.03).max(hem + 0.01);
    let cols = SEGMENTS + 1;
    let mut points = Vec::with_capacity(ys.len() * cols);
    let mut skins = Vec::with_capacity(ys.len() * cols);
    for &y in ys {
        for j in 0..cols {
            let phi = if j == SEGMENTS { 0.0 } else { TAU * fraction(j, SEGMENTS) };
            let edge = top - (spec.rise + depth) * cut.profile(phi);
            let y = if y > floor && top > floor { floor + (y - floor) * (edge - floor) / (top - floor) } else { y };
            let ring = column.ring(y, spec.ease)?;
            let keep = 1.0 - tuck_at(column, spec.tuck, y);
            points.push(ring.centre + (ring.point(phi) - ring.centre) * keep);
            skins.push((ring.centre, skin_at(column, weights, y)));
        }
    }
    let grid = Grid::from_fn(ys.len(), cols, |i, j| points[i * cols + j])?;
    let uvs = tile_uvs(&grid, tile, true);
    let uv = |i: usize, j: usize| uvs.as_ref().map_or(DVec2::ZERO, |u| u[i * grid.cols() + j]);
    let inside = |i: usize, j: usize| skins[i * cols + j].0;
    let skin = |_: DVec3, i: usize, j: usize| skins[i * cols + j].1.clone();
    let options = GridOptions {
        wrap_j: true,
        inside: Some(&inside),
        uv: uvs.is_some().then_some(&uv as &dyn Fn(usize, usize) -> DVec2),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut builder = MeshBuilder::with_colours();
    builder.add_grid(&grid, &options)?;
    Ok(builder)
}

/// Builds the shirt's body and, for a tuck, its lining.
///
/// # Errors
/// [`GarmentError`] if the column has no ring at a height the shirt needs.
pub fn build(
    column: &Column,
    weights: &RegionWeights,
    spec: &TorsoSpec,
    tile: Option<f64>,
) -> Result<Torso, GarmentError> {
    let (bottom, length, top) = (column.bottom(), column.length(), column.top());
    let height = |fraction: f64| bottom + fraction * length;
    let hem = height(spec.hem);
    let [to_waist, to_chest, to_top, up_neck] = ROW_COUNTS;
    let ys = rows_of(&[
        (hem, height(along(REF_WAIST)), to_waist),
        (height(along(REF_WAIST)), height(along(REF_CHEST)), to_chest),
        (height(along(REF_CHEST)), top, to_top),
        (top, top + spec.rise, up_neck),
    ]);
    let row = |y: f64, offset: f64, keep: f64| -> Result<Row, GarmentError> {
        Ok(Row {
            ring: column.ring(y, offset)?,
            keep: keep * (1.0 - tuck_at(column, spec.tuck, y)),
            skin: skin_at(column, weights, y),
        })
    };
    let rows = ys.iter().map(|&y| row(y, spec.ease, 1.0)).collect::<Result<Vec<_>, _>>()?;
    let opening = rows.last().map(|r| r.ring).ok_or_else(|| GarmentError::invalid("shirt", "no rows".to_owned()))?;
    let shirt = match spec.cut.filter(|c| c.depth > 0.0) {
        None => builder_of(&rows, tile)?,
        Some(cut) => cut_builder(column, weights, spec, &cut, &ys, tile)?,
    };
    let lining = if spec.tuck > 0.0 {
        let mut rows: Vec<Row> =
            LINING_POLE.iter().map(|&keep| row(bottom, spec.ease, keep)).collect::<Result<_, _>>()?;
        for y in rows_of(&[(bottom, hem + LINING_OVERLAP, LINING_ROWS)]) {
            rows.push(row(y, spec.ease - LINING_INSET, 1.0)?);
        }
        Some(builder_of(&rows, tile)?)
    } else {
        None
    };
    Ok(Torso { shirt, lining, opening })
}
