//! Braid and rope trim: round strands laid over and under each other along a rail.
//!
//! Strand `k` of `n` winds round the rail on a circle, a phase of `k / n` of a turn ahead of strand 0: across the rail
//! as `R sin(theta)` and up off it as `R (1 + cos(theta))`, `theta` growing by a whole turn each repeat (a lobe) along
//! the rail's length. Seen from above the strands cross from side to side; where two cross, one passes over the other.
//! On an open rail the strands taper to points at both ends; on a closed rail they wrap, and the braid repeats a whole
//! number of times round it.
//!
//! **No strand touches another.** At any point of the rail the strands sit on one circle, `1/n` of a turn apart, and
//! two strands `d` steps apart that are `u` apart in `theta` are `sqrt(c²u² + 4R² sin²((u - 2πd/n) / 2))` apart, where
//! `c` is the lobe's length over a turn: the distance between two helices of one pitch depends only on `u`. The circle's
//! radius `R` is the least for which that distance stays at least [`GAP`] strand widths for every `d` and `u`
//! ([`winding_radius`]). A lobe too short for its strands (`lobe / n` at most that gap) has no such radius, and the
//! build refuses it, naming the lobes.

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
/// The least distance between two strands' centre lines, in strand widths: they never touch, with a twentieth of a
/// width to spare.
pub const GAP: f64 = 1.05;
/// Samples of a strand pair's distance over a turn of `u` when its minimum is sought ([`pair_distance`]).
const PAIR_SAMPLES: usize = 128;
/// Halvings of the search for the winding radius.
const RADIUS_STEPS: usize = 48;
/// The largest winding radius tried, in strand widths, before a braid is refused as too dense.
const MAX_RADIUS: f64 = 64.0;

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
/// [`TrimError::ZeroLengthRail`] for a rail without length; [`TrimError::Invalid`] for a spec that fails validation,
/// or whose lobes are too short for its strands to pass each other.
pub fn build(surface: &Surface, spec: &Braid) -> Result<MeshBuilder, TrimError> {
    checked(spec)?;
    // Fine enough for every repeat to have its samples, and never coarser than the rail asks.
    let length = Rail::length_on(surface, &spec.rail)?;
    let step = (length / (f64::from(spec.lobes) * PER_LOBE)).min(spec.rail.step);
    let rail = Rail::from_polyline_on_surface(surface, &spec.rail, step)?;
    let lobe = rail_length(&rail) / f64::from(spec.lobes);
    let radius = winding_radius(spec.strands, lobe, GAP * spec.strand_width).ok_or_else(|| {
        TrimError::invalid(
            "lobes",
            format!(
                "{} lobes of {:.1} mm leave {:.2} mm a strand along the rail: {} strands {:.1} mm wide need more than \
                 {GAP} widths each; use fewer lobes or a longer rail",
                spec.lobes,
                lobe * 1e3,
                lobe / f64::from(spec.strands) * 1e3,
                spec.strands,
                spec.strand_width * 1e3
            ),
        )
    })?;
    let mut builder = MeshBuilder::new();
    for k in 0..spec.strands {
        builder.merge(&strand(&rail, spec, k, radius)?)?;
    }
    Ok(builder)
}

/// The length of `rail` along its samples, metres: what the lobes divide.
fn rail_length(rail: &Rail) -> f64 {
    rail.points().last().map_or(0.0, |p| p.distance)
}

/// The least distance between two strands of a braid of `strands` wound on a circle of radius `radius` with lobes
/// `lobe` long, `apart` steps apart round it (`0 < apart < strands`): `min over u of sqrt(c²u² + 4R² sin²((u - Δ) / 2))`
/// with `c = lobe / 2π`, `Δ = 2π apart / strands` (see the module docs). Beyond a turn either side, `cu` alone exceeds a lobe; inside, the minimum is
/// found by sampling and a golden-section search round the best sample.
pub fn pair_distance(radius: f64, lobe: f64, apart: u32, strands: u32) -> f64 {
    let per_turn = lobe / TAU;
    let offset = TAU * f64::from(apart) / f64::from(strands);
    let squared = |u: f64| (per_turn * u).powi(2) + (2.0 * radius * ((u - offset) / 2.0).sin()).powi(2);
    let span = 2.0 * TAU;
    let step = span * fraction(1, PAIR_SAMPLES);
    let sample = |k: usize| -TAU + span * fraction(k, PAIR_SAMPLES);
    let best = (0..=PAIR_SAMPLES).map(sample).min_by(|a, b| squared(*a).total_cmp(&squared(*b))).unwrap_or(0.0);
    let (mut lo, mut hi) = (best - step, best + step);
    let golden = 0.5 * (5.0_f64.sqrt() - 1.0);
    for _ in 0..60 {
        let (left, right) = (hi - golden * (hi - lo), lo + golden * (hi - lo));
        if squared(left) < squared(right) {
            hi = right;
        } else {
            lo = left;
        }
    }
    squared(f64::midpoint(lo, hi)).min(squared(best)).sqrt()
}

/// The radius of the circle `strands` strands wind round, lobes `lobe` long, for every two of them to stay at least
/// `gap` apart centre to centre: the least such radius, to within a part in 2^48 of the search's range. `None` when no
/// radius does it, which happens when `lobe / strands` is at most `gap` (two neighbours then meet however wide the
/// circle).
pub fn winding_radius(strands: u32, lobe: f64, gap: f64) -> Option<f64> {
    let clear = |radius: f64| (1..strands).all(|d| pair_distance(radius, lobe, d, strands) >= gap);
    let (mut lo, mut hi) = (0.0, MAX_RADIUS * gap);
    if !(lobe.is_finite() && gap > 0.0 && clear(hi)) {
        return None;
    }
    for _ in 0..RADIUS_STEPS {
        let mid = f64::midpoint(lo, hi);
        if clear(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(hi)
}

/// Strand `k` of the braid along `rail`, wound on a circle of `radius` round it.
#[allow(clippy::many_single_char_names)] // widths, radii and frame axes, as the braid's geometry names them
fn strand(rail: &Rail, spec: &Braid, k: u32, radius: f64) -> Result<MeshBuilder, TrimError> {
    let points = rail.points();
    let r = spec.strand_width / 2.0;
    let phase = TAU * f64::from(k) / f64::from(spec.strands);
    let total = rail_length(rail);
    let last = points.len() - 1;
    let centres: Vec<DVec3> = points
        .iter()
        .map(|p| {
            // By length, not by sample: the rail's samples are uniform in [t, phi], so lobes stay even.
            let theta = TAU * f64::from(spec.lobes) * (p.distance / total) + phase;
            p.point + p.side * (radius * theta.sin()) + p.normal * (r + radius * (1.0 + theta.cos()))
        })
        .collect();
    let closed = rail.closed();
    let thickness = |i: usize| {
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
        let rad = thickness(i);
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
