//! Braid and rope trim: round strands weaving over and under each other along a rail.
//!
//! Strand `k` of `n` follows the rail with a phase of `k / n` of a turn: it swings across the rail as `sin(theta)` and
//! rises and falls as `sin(2 theta)`, so neighbouring strands pass over and under each other where they cross (the
//! three-strand braid, generalised to `n`). On an open rail the strands taper to points at both ends; on a closed rail
//! they wrap, and the braid repeats a whole number of times round it.

use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, ColourRef, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Finish, Rail, RailSpec, TrimError, checked, feature};
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::outfit::loft::Surface;

/// The fewest strands a braid has.
pub const MIN_STRANDS: u32 = 2;
/// The most strands a braid has.
pub const MAX_STRANDS: u32 = 6;
/// The most repeats along a rail.
pub const MAX_LOBES: u32 = 400;
/// Samples along each repeat of the braid, at least.
const PER_LOBE: f64 = 16.0;
/// Segments round a strand.
const ROUND: usize = 8;
/// The share of an open braid's length over which its strands taper to a point at each end.
const TAPER: f64 = 0.04;
/// How far a strand swings across the rail, in strand widths per strand.
const SWING: f64 = 0.4;
/// How far a strand rises and falls, in strand widths.
const RISE: f64 = 0.5;

/// Strands braided along a rail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Braid {
    /// Where it runs.
    pub rail: RailSpec,
    /// How many strands, from [`MIN_STRANDS`] to [`MAX_STRANDS`].
    pub strands: u32,
    /// How many times the braid repeats along the rail, 1 to [`MAX_LOBES`].
    pub lobes: u32,
    /// The thickness of a strand, metres.
    pub strand_width: f64,
    /// What it is made of.
    #[serde(default = "gold_cord")]
    pub finish: Finish,
}

fn gold_cord() -> Finish {
    Finish::cloth(ColourRef::rgb(0xc8, 0x9a, 0x3c))
}

impl Validate for Braid {
    fn check(&self, c: &mut Check) {
        c.nested("rail", &self.rail);
        if !(MIN_STRANDS..=MAX_STRANDS).contains(&self.strands) {
            c.fail("strands", format!("{}: a braid has {MIN_STRANDS} to {MAX_STRANDS} strands", self.strands));
        }
        if !(1..=MAX_LOBES).contains(&self.lobes) {
            c.fail("lobes", format!("{}: a braid repeats 1 to {MAX_LOBES} times", self.lobes));
        }
        feature(c, "strand_width", self.strand_width);
        c.nested("finish", &self.finish);
    }
}

/// The braid `spec` describes on `surface`: one closed tube per strand.
///
/// # Errors
/// [`TrimError::ZeroLengthRail`] for a rail without length; [`TrimError::Invalid`] for a spec that fails validation.
pub fn build(surface: &Surface, spec: &Braid) -> Result<MeshBuilder, TrimError> {
    checked(spec)?;
    // Fine enough for every repeat to have its samples, and never coarser than the rail asks.
    let length = Rail::length_on(surface, &spec.rail)?;
    let step = (length / (f64::from(spec.lobes) * PER_LOBE)).min(spec.rail.step);
    let rail = Rail::from_polyline_on_surface(surface, &spec.rail, step)?;
    let mut builder = MeshBuilder::new();
    for k in 0..spec.strands {
        builder.merge(&strand(&rail, spec, k)?)?;
    }
    Ok(builder)
}

/// Strand `k` of the braid along `rail`.
#[allow(clippy::many_single_char_names)] // widths, radii and frame axes, as the braid's geometry names them
fn strand(rail: &Rail, spec: &Braid, k: u32) -> Result<MeshBuilder, TrimError> {
    let points = rail.points();
    let (w, r) = (spec.strand_width, spec.strand_width / 2.0);
    let swing = SWING * w * f64::from(spec.strands);
    let phase = TAU * f64::from(k) / f64::from(spec.strands);
    let last = points.len() - 1;
    let centres: Vec<DVec3> = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let theta = TAU * f64::from(spec.lobes) * fraction(i, last) + phase;
            let up = r + RISE * w * 0.5 * (1.0 + (2.0 * theta).sin());
            p.point + p.side * (swing * theta.sin()) + p.normal * up
        })
        .collect();
    let closed = rail.closed();
    let radius = |i: usize| {
        if closed {
            return r;
        }
        let u = fraction(i, last);
        r * (u.min(1.0 - u) / TAPER).min(1.0).sqrt()
    };
    // Each ring is square to the strand's own direction, its first point toward the surface normal, so a closed
    // braid's rings repeat exactly at the seam.
    let mut rings = Vec::with_capacity(centres.len() * (ROUND + 1));
    for (i, c) in centres.iter().enumerate() {
        let (before, after) = match (i, closed) {
            (0, true) => (last - 1, 1),
            (i, true) if i == last => (last - 1, 1),
            (0, false) => (0, 1),
            (i, false) if i == last => (last - 1, last),
            (i, _) => (i - 1, i + 1),
        };
        let tangent = (centres[after] - centres[before]).try_normalize().ok_or_else(|| {
            TrimError::invalid("braid", "a strand doubles back on itself: use fewer lobes or a longer rail")
        })?;
        let up = points[i].normal;
        let n = (up - tangent * up.dot(tangent)).try_normalize().unwrap_or_else(|| tangent.any_orthonormal_vector());
        let b = tangent.cross(n);
        let rad = radius(i);
        for j in 0..=ROUND {
            let a = TAU * fraction(j % ROUND, ROUND);
            rings.push(*c + (n * a.cos() + b * a.sin()) * rad);
        }
    }
    let grid = Grid::new(centres.len(), ROUND + 1, rings)?;
    let inside = |i: usize, _: usize| centres[i];
    let skin = |_: DVec3, i: usize, _: usize| points[i].skin.clone();
    let options = GridOptions {
        wrap_i: closed,
        wrap_j: true,
        inside: Some(&inside),
        skin: Skin::Per(&skin),
        ..GridOptions::default()
    };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&grid, &options)?;
    Ok(builder)
}
