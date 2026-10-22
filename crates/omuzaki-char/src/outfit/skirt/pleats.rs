//! The skirt's pleated surface: the outer and inner skins of the knife-pleated skirt, from the envelope's profile.
//!
//! The port of `buildSkirt` (`uniform.js` 322-376) as the Director's reference for #192 gives it. The skirt is a grid of
//! 12 rows by `count x fold + 1` columns round the waist: every column sits on the base ellipse of the profile (`rx`, `rz`
//! and `cz` against height) at its angle, pushed *in* by the fold's depth, so the base ellipse is the line along the
//! pleats' crests and the folds only go inwards. The pleat cross-section is a long gentle slope and a sharp return, the
//! knife-pleat asymmetry. The pleats fade out to nothing at the waist and grow toward the hem; the valleys ride a
//! little higher than the crests, so the hem is wavy. The inner skin is the outer one pulled in by the cloth's thickness
//! along the horizontal normal.
//!
//! Heights are written as fractions `s` of the way from the hem (`0`) to the waist (`1`), and so are the profile's knots:
//! a monotone cubic through knots is unchanged by a linear change of its coordinate, so the shape is the prototype's at
//! every hem and waist height.

use omuzaki_core::glam::{DVec2, DVec3};

use crate::kit::KitError;
use crate::kit::curve::{Pchip, TAU, lerp, sstep};
use crate::kit::grid::{Grid, GridNormals, GridOptions, grid_normals};

/// The rows of the grid, as fractions of the way from the waist (`0`) to the hem (`1`) (`S` in `buildSkirt`).
pub const ROWS: [f64; 12] = [0.0, 0.05, 0.11, 0.17, 0.25, 0.35, 0.46, 0.58, 0.70, 0.81, 0.91, 1.0];
/// Where the first pleat starts, as a fraction of a pleat period before the front (`ph0 = -0.42 d`).
pub const PHASE: f64 = -0.42;
/// How much higher a valley's hem rides than a crest's, metres per unit of fold depth (`hemOf`).
pub const VALLEY_LIFT: f64 = 0.0017;
/// The cloth's thickness: how far the inner skin lies inside the outer one, metres (`SK_THICK`).
pub const THICKNESS: f64 = 0.0030;
/// Where the pleats fade out toward the waist, as heights fraction `s` from the hem (placeholders: the sample body's
/// `sstep(0.372, 0.336, y)` against a hem at `0.2575` and a waist at `0.396`): none above the first, full below the second.
pub const FADE_NONE: f64 = (0.372 - PROTOTYPE_HEM) / (PROTOTYPE_TOP - PROTOTYPE_HEM);
/// See [`FADE_NONE`].
pub const FADE_FULL: f64 = (0.336 - PROTOTYPE_HEM) / (PROTOTYPE_TOP - PROTOTYPE_HEM);
/// Where the pleats finish growing toward the hem (`lerp(0.9, 1.12, sstep(0.34, 0.26, y))`), as `s`: placeholders.
pub const GROW_NONE: f64 = (0.34 - PROTOTYPE_HEM) / (PROTOTYPE_TOP - PROTOTYPE_HEM);
/// See [`GROW_NONE`].
pub const GROW_FULL: f64 = (0.26 - PROTOTYPE_HEM) / (PROTOTYPE_TOP - PROTOTYPE_HEM);
/// The depth factor of the pleats at the waist side of the growth and at the hem side of it.
pub const GROW_FROM: f64 = 0.9;
/// See [`GROW_FROM`].
pub const GROW_TO: f64 = 1.12;
/// The prototype's skirt waist and hem heights on its own body, metres: the frame the placeholder fractions are in.
pub const PROTOTYPE_TOP: f64 = 0.396;
/// See [`PROTOTYPE_TOP`].
pub const PROTOTYPE_HEM: f64 = 0.2575;
/// The reference height the texture's `v` counts down from, as `s` (placeholder: `0.376` on the sample body).
pub const V_REFERENCE: f64 = (0.376 - PROTOTYPE_HEM) / (PROTOTYPE_TOP - PROTOTYPE_HEM);
/// Where the outer colour is brightest, toward the hem: none above the first `s`, full below the second (placeholders).
pub const SHADE_TOP: f64 = (0.37 - PROTOTYPE_HEM) / (PROTOTYPE_TOP - PROTOTYPE_HEM);
/// See [`SHADE_TOP`].
pub const SHADE_BOTTOM: f64 = (0.30 - PROTOTYPE_HEM) / (PROTOTYPE_TOP - PROTOTYPE_HEM);
/// How much a valley darkens the cloth, and the exponent of that darkening.
pub const VALLEY_DARKEN: f64 = 0.30;
/// See [`VALLEY_DARKEN`].
pub const VALLEY_EXPONENT: f64 = 1.2;
/// The colour of the inner skin and the inside of the hem.
pub const INNER_TONE: [f32; 3] = [0.62, 0.66, 0.64];
/// A tile of the plaid is this long in metres, both ways: the texture repeats every 50 mm.
pub const TILE: f64 = 0.05;
/// The interior reference point of the skirt's normals is this far in front of its axis (`inside`), metres.
const INSIDE_Z: f64 = 0.002;

/// One column of the grid: its angle round the waist, and how deep into the fold it is (`0` on a crest, `-1` in the
/// deepest valley).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Column {
    /// The angle, `0` at the front, `pi / 2` at the character's left.
    pub phi: f64,
    /// The fold factor, `-1..=0`.
    pub fold: f64,
}

/// The columns of `count` pleats cut as `fold` (`(p, f)` pairs: `p` the fraction of the pleat period, `f` the fold
/// factor), and the closing column that repeats the first.
pub fn columns(count: usize, fold: &[[f64; 2]]) -> Vec<Column> {
    #[allow(clippy::cast_precision_loss)] // a pleat count is tiny
    let period = TAU / count as f64;
    let start = PHASE * period;
    let mut out = Vec::with_capacity(count * fold.len() + 1);
    for m in 0..count {
        #[allow(clippy::cast_precision_loss)]
        let m = m as f64;
        out.extend(fold.iter().map(|&[p, f]| Column { phi: start + (m + p) * period, fold: f }));
    }
    #[allow(clippy::cast_precision_loss)]
    out.push(Column { phi: start + count as f64 * period, fold: 0.0 });
    out
}

/// How deep pleats of `depth` are at fraction `s` of the way from the hem to the waist (`dAmp`): none at the waist, and a
/// little deeper toward the hem.
pub fn amplitude_at(depth: f64, s: f64) -> f64 {
    depth * sstep(FADE_NONE, FADE_FULL, s) * lerp(GROW_FROM, GROW_TO, sstep(GROW_NONE, GROW_FULL, s))
}

/// The envelope the pleats are cut in: the base ellipse by height, and the heights it spans.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The waist's height, metres.
    pub top: f64,
    /// The hem's height (a crest's), metres.
    pub hem: f64,
    rx: Pchip,
    rz: Pchip,
    cz: Pchip,
    depth: f64,
}

/// A point of the base ellipse with its horizontal outward normal.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Base {
    /// The point's x.
    pub x: f64,
    /// The point's z.
    pub z: f64,
    /// The normal's x.
    pub nx: f64,
    /// The normal's z.
    pub nz: f64,
}

impl Plan {
    /// The plan of an envelope between `hem` and `top` metres with the profile `rx`, `rz` and `cz` (knots `(s, value)`
    /// with `s` from the hem to the waist) and pleats `depth` deep.
    ///
    /// # Errors
    /// [`KitError`] for a profile curve the kit refuses (no knots, repeated knots, or a value that isn't finite).
    pub fn new(hem: f64, top: f64, [rx, rz, cz]: [&[[f64; 2]]; 3], depth: f64) -> Result<Self, KitError> {
        let curve = |knots: &[[f64; 2]]| Pchip::new(&knots.iter().map(|&[s, v]| (s, v)).collect::<Vec<_>>());
        Ok(Self { top, hem, rx: curve(rx)?, rz: curve(rz)?, cz: curve(cz)?, depth })
    }

    /// The height at fraction `s` of the way from the hem to the waist.
    pub fn height(&self, s: f64) -> f64 {
        lerp(self.hem, self.top, s)
    }

    /// The fraction `s` of the way from the hem to the waist that height `y` is at.
    pub fn fraction(&self, y: f64) -> f64 {
        (y - self.hem) / (self.top - self.hem)
    }

    /// The base ellipse at height `y`, angle `phi`.
    pub fn base(&self, y: f64, phi: f64) -> Base {
        let s = self.fraction(y);
        let (rx, rz, cz) = (self.rx.eval(s), self.rz.eval(s), self.cz.eval(s));
        let (sin, cos) = phi.sin_cos();
        let (gx, gz) = (sin / rx, cos / rz);
        let length = gx.hypot(gz);
        Base { x: sin * rx, z: cz + cos * rz, nx: gx / length, nz: gz / length }
    }

    /// How deep the pleats are at height `y`, metres: none toward the waist, and a little deeper toward the hem.
    pub fn amplitude(&self, y: f64) -> f64 {
        amplitude_at(self.depth, self.fraction(y))
    }

    /// The hem of the column `column`: a valley's rides higher.
    pub fn hem_of(&self, column: &Column) -> f64 {
        self.hem - VALLEY_LIFT * column.fold
    }

    /// The point of the outer skin at height `y` in `column`.
    pub fn outer_point(&self, y: f64, column: &Column) -> DVec3 {
        let base = self.base(y, column.phi);
        let off = self.amplitude(y) * column.fold;
        DVec3::new(base.x + base.nx * off, y, base.z + base.nz * off)
    }
}

/// The outer skin's grid: `ROWS` by `columns`.
///
/// # Errors
/// [`KitError`] for a grid the kit refuses.
pub fn outer_grid(plan: &Plan, columns: &[Column]) -> Result<Grid, KitError> {
    Grid::from_fn(ROWS.len(), columns.len(), |i, j| {
        let y = lerp(plan.top, plan.hem_of(&columns[j]), ROWS[i]);
        plan.outer_point(y, &columns[j])
    })
}

/// The inside reference of the skirt's normals at a vertex of height `y`: a point on the axis, a hair in front (`inside`).
pub fn inside(y: f64) -> DVec3 {
    DVec3::new(0.0, y, INSIDE_Z)
}

/// The outer skin with its smooth normals, and the inner skin pulled in by [`THICKNESS`] along the horizontal normal.
pub struct Skins {
    /// The outer skin.
    pub outer: Grid,
    /// Its normals, one per point.
    pub normals: GridNormals,
    /// The inner skin.
    pub inner: Grid,
}

/// The two skins of the skirt.
///
/// # Errors
/// [`KitError`] as [`outer_grid`] and [`grid_normals`].
pub fn skins(plan: &Plan, columns: &[Column]) -> Result<Skins, KitError> {
    let outer = outer_grid(plan, columns)?;
    let cols = outer.cols();
    let at = |k: usize| inside(outer.points()[k].y);
    let reference = |i: usize, j: usize| at(i * cols + j);
    let options = GridOptions { wrap_j: true, inside: Some(&reference), ..GridOptions::default() };
    let normals = grid_normals(&outer, &options)?;
    let inner = Grid::from_fn(outer.rows(), cols, |i, j| {
        let (p, n) = (outer.at(i, j), normals.normals[i * cols + j]);
        let horizontal = DVec2::new(n.x, n.z);
        let length = horizontal.length();
        let unit = if length == 0.0 { horizontal } else { horizontal / length };
        DVec3::new(p.x - unit.x * THICKNESS, p.y, p.z - unit.y * THICKNESS)
    })?;
    Ok(Skins { outer, normals, inner })
}

/// The horizontal arc length along the hem row of `outer`, from its first column: the plaid's `u` (metres).
pub fn arc_lengths(outer: &Grid) -> Vec<f64> {
    let last = outer.rows() - 1;
    let mut u = vec![0.0; outer.cols()];
    for j in 1..outer.cols() {
        let (a, b) = (outer.at(last, j - 1), outer.at(last, j));
        u[j] = u[j - 1] + (b.x - a.x).hypot(b.z - a.z);
    }
    u
}
