//! The curtain's strip geometry (`shellFill`, `parts/hair.js` 421 to 494).
//!
//! **One wide sheet, split into clumps.** Each guide owns [`column_count`] columns of vertices running down its length,
//! at offsets `a` from -1 to 1 across it (`a_i = -1 + 2 i / (columns - 1)`, which is the prototype's
//! `[-1, -0.5, 0, 0.5, 1]` for 5; a single column is `a = 0` alone). A column at `a != 0` is the guide's own point pulled
//! `|a| / 2` of the way toward the neighbouring guide on its side (the missing neighbour mirrored at the ends of the
//! curtain), so the outer columns of neighbours meet halfway. Columns end at `1 - 0.15 |a|^1.5` of the length, so the
//! bottom splits into pointed clumps, and every column bulges outward by `0.007 (1 - a^2) ss(t, 0.04, 0.25)` (zero only
//! at the outer edge).
//!
//! **Separate vertices per strip.** Neighbouring strips share no vertices, so each clump shades on its own; `hair-info`
//! carries a seed per strip. Triangles join the columns of one guide only.
//!
//! **Winding.** The triangles face outward, tested on the middle strip's centre column against the outward direction
//! there; one decision serves every strip, because their column and length directions turn together round the head.
//!
//! **Folds.** Neighbouring guides carry independent waves, so lower down they can cross and the strip between them
//! folds over itself (#841). [`Spacing::Prototype`] keeps that as the prototype draws it; [`Spacing::Clamped`] holds
//! each column's sideways distance from its guide above [`MIN_GAP`], so a column never passes its guide.
//!
//! **Two entry points.** [`strip_positions`] writes into a caller-owned slice without allocating (the runtime calls it
//! each step on the posed guides); [`strip_mesh`] builds the whole mesh at rest with the attributes the `hair` shading
//! program reads.

use omuzaki_char_api::{ExtraAttribute, HAIR_INFO, HAIR_UV};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::{Slug, SlugError};
use omuzaki_core::mesh::MeshData;

use crate::hair::tube::{along, smoothstep};

/// How far the columns bulge outward, metres.
const BULGE: f32 = 0.007;
/// The bulge grows in over this span of the strip's length ...
const BULGE_FROM: f32 = 0.04;
/// ... to here.
const BULGE_TO: f32 = 0.25;
/// How much shorter than the guide the outermost columns end.
const EDGE_SHORTENING: f32 = 0.15;
/// Just short of the guide's last point, so sampling never reads past it.
const LAST_SAMPLE: f32 = 0.9999;
/// Sideways distance a clamped column keeps from its guide's line to its neighbour's, metres: a quarter of the usual
/// 1.6 cm spacing between guides, so a clamped fold flattens to a seam rather than collapsing.
pub const MIN_GAP: f32 = 0.004;
/// The hair-info seed stride per strip, and its offset (the prototype's `(j * 0.371 + 0.13) % 1`).
const SEED_STRIDE: f32 = 0.371;
const SEED_OFFSET: f32 = 0.13;
/// The shade of strip `j` is `0.97 + 0.03 sin(2.1 j)`.
const SHADE_BASE: f32 = 0.97;
const SHADE_SWING: f32 = 0.03;
const SHADE_STRIDE: f32 = 2.1;

/// How the sideways distance between a column and its neighbouring guide is treated where guides cross.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Spacing {
    /// As the prototype draws it: a column follows its neighbour wherever it goes, so crossing guides fold the strip.
    #[default]
    Prototype,
    /// A column is never pulled past its own guide: the pull is limited so the sideways distance stays at least
    /// [`MIN_GAP`].
    Clamped,
}

/// Why a strip could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StripError {
    /// A guide needs two points to have a length.
    #[error("a guide needs at least 2 points, got {0}")]
    TooFewPoints(usize),
    /// Every guide of a curtain has the same number of points.
    #[error("guide {guide} has {got} points but the first has {expected}")]
    Ragged {
        /// The odd guide, in curtain order.
        guide: usize,
        /// The first guide's point count.
        expected: usize,
        /// This guide's.
        got: usize,
    },
    /// A curtain needs one position across the back per guide.
    #[error("{clumps} guides but {u} positions across the back")]
    GuideCount {
        /// Guides.
        clumps: usize,
        /// Positions.
        u: usize,
    },
    /// The output slice is not one vertex per column per point per guide.
    #[error("the strip has {expected} vertices, but the output holds {got}")]
    WrongOutputLength {
        /// `guides * columns * points`.
        expected: usize,
        /// What the caller gave.
        got: usize,
    },
    /// More vertices than a mesh can index.
    #[error("a mesh can hold at most 2^32 vertices")]
    TooMany,
    /// A contract attribute name stopped being a valid slug.
    #[error("attribute name: {0}")]
    Name(#[from] SlugError),
}

/// The columns a strip has for `columns` asked for. The count is the caller's: one column is a line with no width.
pub fn column_count(columns: u8) -> usize {
    usize::from(columns)
}

/// The offset `a` of column `i` of `count`, from -1 to 1 (0 for a lone column).
#[allow(clippy::cast_precision_loss)] // a handful of columns
fn column_offset(i: usize, count: usize) -> f32 {
    if count < 2 { 0.0 } else { -1.0 + 2.0 * i as f32 / (count - 1) as f32 }
}

/// The guide indices in curtain order: ascending `u`, so neighbours in the list are neighbours across the back. Ties
/// keep their input order.
pub fn curtain_order(u: &[f32]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..u.len()).collect();
    order.sort_by(|&a, &b| u[a].total_cmp(&u[b]));
    order
}

/// Vertices in a strip of `guides` guides of `points` points.
pub fn vertex_count(guides: usize, points: usize, columns: u8) -> usize {
    guides * column_count(columns) * points
}

/// The common point count of the guides, checked.
fn common_points(guides: &[&[Vec3]]) -> Result<usize, StripError> {
    let points = guides.first().map_or(0, |g| g.len());
    if !guides.is_empty() && points < 2 {
        return Err(StripError::TooFewPoints(points));
    }
    match guides.iter().enumerate().find(|(_, g)| g.len() != points) {
        Some((guide, g)) => Err(StripError::Ragged { guide, expected: points, got: g.len() }),
        None => Ok(points),
    }
}

/// The point `fraction` of the way along `guide` (clamped just short of the end), by linear interpolation. The guide
/// has at least two points (checked by the caller); a non-finite fraction reads the root.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few dozen points
fn sample(guide: &[Vec3], fraction: f32) -> Vec3 {
    let fraction = if fraction.is_finite() { fraction.clamp(0.0, LAST_SAMPLE) } else { 0.0 };
    let at = fraction * (guide.len() - 1) as f32;
    let i = (at as usize).min(guide.len() - 2);
    guide[i].lerp(guide[i + 1], at - i as f32)
}

/// The horizontal direction across the curtain at guide `j`: from the root before it to the root after it (one-sided at
/// the ends), the way the guides' `u` runs. Falls back to +X for a lone or degenerate guide.
fn across_axis(guides: &[&[Vec3]], j: usize) -> Vec3 {
    let (before, after) = (guides[j.saturating_sub(1)][0], guides[(j + 1).min(guides.len() - 1)][0]);
    let flat = Vec3::new(after.x - before.x, 0.0, after.z - before.z);
    flat.try_normalize().unwrap_or(Vec3::X)
}

/// Where the column at offset `a` of guide `j` passes at `fraction` of the way down, before the bulge: the guide's own
/// point, pulled toward its neighbour on that side.
fn column_point(guides: &[&[Vec3]], j: usize, a: f32, t: f32, spacing: Spacing) -> Vec3 {
    let here = sample(guides[j], t);
    if a == 0.0 {
        return here;
    }
    let towards_lower = a < 0.0;
    let neighbour = if towards_lower { j.checked_sub(1) } else { Some(j + 1).filter(|&n| n < guides.len()) };
    let mut there = match neighbour {
        Some(n) => sample(guides[n], t),
        // The missing neighbour is the one on the other side, mirrored through this guide. A lone guide has none: no pull.
        None => match if towards_lower { Some(j + 1) } else { j.checked_sub(1) }.filter(|&n| n < guides.len()) {
            Some(n) => here + (here - sample(guides[n], t)),
            None => return here,
        },
    };
    if spacing == Spacing::Clamped {
        // `side` points from this guide toward the neighbour; the neighbour must stay at least MIN_GAP that way.
        let side = across_axis(guides, j) * if towards_lower { -1.0 } else { 1.0 };
        let gap = (there - here).dot(side);
        if gap < MIN_GAP {
            there += side * (MIN_GAP - gap);
        }
    }
    here.lerp(there, a.abs() * 0.5)
}

/// Writes the strip's vertex positions into `out`, column after column, guide after guide: vertex
/// `(j * columns + c) * points + k` is point `k` of column `c` of guide `j`. Allocates nothing. `guides` are in curtain
/// order; `outward` gives the direction the strip bulges toward.
///
/// # Errors
/// [`StripError::TooFewPoints`] or [`StripError::Ragged`] for guides that cannot make a strip,
/// [`StripError::WrongOutputLength`] if `out` is not [`vertex_count`] long.
pub fn strip_positions(
    guides: &[&[Vec3]],
    columns: u8,
    spacing: Spacing,
    outward: impl Fn(Vec3) -> Vec3,
    out: &mut [Vec3],
) -> Result<(), StripError> {
    let points = common_points(guides)?;
    let expected = vertex_count(guides.len(), points, columns);
    if out.len() != expected {
        return Err(StripError::WrongOutputLength { expected, got: out.len() });
    }
    let count = column_count(columns);
    let mut slots = out.iter_mut();
    for j in 0..guides.len() {
        for c in 0..count {
            let a = column_offset(c, count);
            let shorten = 1.0 - EDGE_SHORTENING * a.abs().powf(1.5);
            for k in 0..points {
                let t = along(k, points - 1) * shorten;
                let mut p = column_point(guides, j, a, t, spacing);
                p += outward(p) * (BULGE * (1.0 - a * a) * smoothstep(t, BULGE_FROM, BULGE_TO));
                if let Some(slot) = slots.next() {
                    *slot = p;
                }
            }
        }
    }
    Ok(())
}

/// The triangle list of a strip of `guides` guides of `points` points: bands between neighbouring columns of one guide
/// (never between guides), wound so they face the way `flip` says (see [`winding_flips`]).
///
/// # Errors
/// [`StripError::TooMany`] if the strip has more vertices than a mesh can index.
pub fn strip_indices(guides: usize, points: usize, columns: u8, flip: bool) -> Result<Vec<u32>, StripError> {
    let count = column_count(columns);
    let total = u32::try_from(vertex_count(guides, points, columns)).map_err(|_| StripError::TooMany)?;
    let (points, count) = (u32_of(points), u32_of(count));
    let mut indices = Vec::new();
    for j in 0..u32_of(guides) {
        for c in 0..count.saturating_sub(1) {
            for k in 0..points - 1 {
                let a0 = (j * count + c) * points + k;
                let b0 = a0 + points;
                if flip {
                    indices.extend([a0, a0 + 1, b0, b0, a0 + 1, b0 + 1]);
                } else {
                    indices.extend([a0, b0, a0 + 1, b0, b0 + 1, a0 + 1]);
                }
            }
        }
    }
    debug_assert!(indices.iter().all(|&i| i < total));
    Ok(indices)
}

/// A small count as `u32`; the callers have already checked the whole mesh fits.
#[allow(clippy::cast_possible_truncation)] // checked against the vertex count first
fn u32_of(n: usize) -> u32 {
    n as u32
}

/// Whether the strips' triangles must be flipped to face outward: the prototype tests the middle guide's middle column
/// at its middle point, where the column's direction down, the direction to the next column, and the outward direction
/// are all well defined.
pub fn winding_flips(
    positions: &[Vec3],
    guides: usize,
    points: usize,
    columns: u8,
    outward: impl Fn(Vec3) -> Vec3,
) -> bool {
    let count = column_count(columns);
    if count < 2 || guides == 0 {
        return false;
    }
    let base = ((guides / 2) * count + (count - 1) / 2) * points + points / 2;
    let (Some(&p0), Some(&down), Some(&across)) =
        (positions.get(base), positions.get(base + 1), positions.get(base + points))
    else {
        return false;
    };
    (across - p0).cross(down - p0).dot(outward(p0)) < 0.0
}

/// Smooth vertex normals: each triangle's area-weighted normal added to its three vertices (three.js
/// `computeVertexNormals`), then normalised; a vertex of no triangle gets +Z.
pub fn strip_normals(positions: &[Vec3], indices: &[u32]) -> Vec<Vec3> {
    let mut sums = vec![Vec3::ZERO; positions.len()];
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = tri.map(|i| i as usize);
        let (Some(&pa), Some(&pb), Some(&pc)) = (positions.get(a), positions.get(b), positions.get(c)) else {
            continue;
        };
        let face = (pc - pb).cross(pa - pb);
        for i in [a, b, c] {
            sums[i] += face;
        }
    }
    sums.into_iter().map(|n| n.try_normalize().unwrap_or(Vec3::Z)).collect()
}

/// What each strip looks like beyond its geometry.
#[derive(Copy, Clone, Debug)]
pub struct StripLook {
    /// The shade multiplier of the clump (1 for the prototype's hair), folded into `hair-info.y`.
    pub shade: f32,
    /// The vertex colour: the clump's streak tint.
    pub colour: [f32; 4],
}

/// The whole strip mesh at rest. `guides` are in curtain order and `looks` has one entry per guide; `head` is the head
/// centre, from which `hair-info` takes its polar angle.
///
/// # Errors
/// As [`strip_positions`], plus [`StripError::GuideCount`] if `looks` and `guides` differ in length and
/// [`StripError::TooMany`] if the mesh would need more than 2^32 vertices.
pub fn strip_mesh(
    guides: &[&[Vec3]],
    looks: &[StripLook],
    columns: u8,
    spacing: Spacing,
    head: Vec3,
    outward: impl Fn(Vec3) -> Vec3,
) -> Result<MeshData, StripError> {
    if looks.len() != guides.len() {
        return Err(StripError::GuideCount { clumps: guides.len(), u: looks.len() });
    }
    let points = common_points(guides)?;
    let count = column_count(columns);
    let mut positions = vec![Vec3::ZERO; vertex_count(guides.len(), points, columns)];
    strip_positions(guides, columns, spacing, &outward, &mut positions)?;
    let flip = winding_flips(&positions, guides.len(), points, columns, &outward);
    let indices = strip_indices(guides.len(), points, columns, flip)?;
    let normals = strip_normals(&positions, &indices);
    let (mut uv, mut info) = (Vec::with_capacity(positions.len() * 2), Vec::with_capacity(positions.len() * 3));
    let (mut uvs, mut colours) = (Vec::with_capacity(positions.len()), Vec::with_capacity(positions.len()));
    for (j, (guide, look)) in guides.iter().zip(looks).enumerate() {
        let seed = (u_f32(j) * SEED_STRIDE + SEED_OFFSET).fract();
        let shade = (SHADE_BASE + SHADE_SWING * (u_f32(j) * SHADE_STRIDE).sin()) * look.shade;
        for c in 0..count {
            let a = column_offset(c, count);
            for k in 0..points {
                let t = along(k, points - 1);
                // The guide's own point sets the polar angle, as in the prototype, not the column's.
                let polar = (guide[k] - head).normalize_or_zero().y.clamp(-1.0, 1.0).acos();
                uv.extend([a, t]);
                info.extend([seed, shade, polar]);
                uvs.push(Vec2::new(0.5 + 0.5 * a, t));
                colours.push(look.colour);
            }
        }
    }
    Ok(MeshData {
        positions,
        normals,
        uvs,
        colours,
        extra: vec![
            ExtraAttribute { name: Slug::new(HAIR_UV)?, components: 2, data: uv },
            ExtraAttribute { name: Slug::new(HAIR_INFO)?, components: 3, data: info },
        ],
        indices,
        ..MeshData::default()
    })
}

#[allow(clippy::cast_precision_loss)] // up to a few hundred strips
fn u_f32(n: usize) -> f32 {
    n as f32
}
