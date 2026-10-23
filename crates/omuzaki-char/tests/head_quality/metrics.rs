//! The four head-quality metrics, computed on a built head mesh (Live grid, bind space).
//!
//! - **crease**: the largest jump, between neighbouring grid vertices, of the angle the surface turns through along a
//!   grid line (a column from the crown to the chin, or a ring round the head), anywhere in the lower face and across
//!   the seam where it meets the cranium. A smooth surface turns by nearly the same angle at neighbouring vertices; a
//!   crease or a notch turns sharply at one vertex. Degrees.
//! - **flange**: how far the front silhouette strays from the target outline, from the outline's top to the chin: the
//!   Hausdorff distance between the two curves, both sides of the face. A saucer at jaw height, a chin that stops short
//!   or overshoots, or a lower face that ignores the outline all show here. Millimetres.
//! - **spike**: the chin tip angle, the smaller of the front and the side view: the angle of the silhouette at its lowest
//!   point, between the two places where it crosses a circle of radius [`TIP_SCALE`] x the outline's half-width round
//!   that point. A beak or a stalk is a small angle; a round chin is near 180. Degrees.
//! - **ripple**: normal variation along vertical lines: per column, the root-mean-square angle between each vertex
//!   normal and the mean of its neighbours above and below, over the lower face; the worst column. A staircase in the
//!   width profile shows as shading ripples here while each step is too small to read as a crease. Degrees.
//!
//! "The lower face" is every vertex at or below the outline's top plus [`SEAM_MARGIN`]; the two rings at the bottom
//! pole, where the grid closes, are left out of the crease and ripple.

use std::f64::consts::PI;

use omuzaki_char::head::mesh::Resolution;
use omuzaki_char::head::outline::{MAX_BAND, OutlineCurve};
use omuzaki_char_api::{HeadOutline, HeadParams};
use omuzaki_core::glam::{DVec2, DVec3};
use omuzaki_core::mesh::MeshData;

/// How far above the outline's top the crease and ripple look, m: the tallest blend band (`outline::MAX_BAND`) and 2 cm
/// more, so the whole seam with the cranium is inside the region. The band used to be 3 cm and this 6 cm.
pub const SEAM_MARGIN: f64 = MAX_BAND + 0.02;
/// The chin-tip circle's radius as a fraction of the outline's half-width at its top.
pub const TIP_SCALE: f64 = 0.25;
/// Height step of the silhouette slices, m.
const SLICE: f64 = 0.000_25;
/// Pixel of the chin-tip raster, m.
const TIP_PIXEL: f64 = 0.000_2;

/// The outline the lower face should follow: the outline's curve (`outline::OutlineCurve`) from `top`, where it is
/// `half_width` wide, closing at `chin`. For a head with a chin this is the outline stretched down to the chin, as the
/// chin stretch stretches the face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    /// Half-width at the top, m.
    pub half_width: f64,
    /// Head-local height of the top, m.
    pub top: f64,
    /// Head-local height of the chin, where the outline closes, m.
    pub chin: f64,
    curve: OutlineCurve,
}

impl Target {
    /// The target an outline asks for.
    pub fn of(outline: &HeadOutline) -> Self {
        Self {
            half_width: outline.half_width,
            top: outline.top,
            chin: outline.chin_y(),
            curve: OutlineCurve::of(outline),
        }
    }

    /// Half-width at head-local height `y` (full above the top, 0 below the chin).
    pub fn half_width_at(&self, y: f64) -> f64 {
        self.curve.at(y)
    }

    /// Dense points of the right half from the top to the chin, about [`SLICE`] apart along the curve (it runs nearly
    /// flat into the chin, so steps in height alone would leave that stretch sparse).
    fn points(&self) -> Vec<DVec2> {
        let length = (self.top - self.chin).max(0.0);
        let fine = steps(length, SLICE / 64.0);
        let mut out = vec![DVec2::new(self.half_width_at(self.top), self.top)];
        for i in 1..=fine {
            let y = self.top - length * fraction(i, fine);
            let point = DVec2::new(self.half_width_at(y), y);
            if i == fine || out.last().is_some_and(|last| last.distance(point) >= SLICE) {
                out.push(point);
            }
        }
        out
    }
}

/// The four metrics of one head, with where the worst crease and ripple are (for reading a sheet).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quality {
    /// Degrees.
    pub crease: f64,
    /// Millimetres.
    pub flange: f64,
    /// Degrees: the smaller of `front_tip` and `side_tip`.
    pub spike: f64,
    /// Degrees.
    pub ripple: f64,
    /// Chin tip angle in the front view, degrees.
    pub front_tip: f64,
    /// Chin tip angle in the side view, degrees.
    pub side_tip: f64,
    /// Head-local height of the worst crease, m.
    pub crease_y: f64,
    /// Azimuth of the worst crease from the front, degrees.
    pub crease_phi: f64,
    /// Head-local height of the lowest vertex, m.
    pub lowest: f64,
}

/// Measures `mesh`, built from `params` at `grid`, against `target`.
pub fn measure(mesh: &MeshData, grid: Resolution, params: &HeadParams, target: &Target) -> Quality {
    let centre = DVec3::from_array(params.cranium.centre());
    let points: Vec<DVec3> = mesh.positions.iter().map(|p| p.as_dvec3() - centre).collect();
    let normals: Vec<DVec3> = mesh.normals.iter().map(|n| n.as_dvec3()).collect();
    let g = GridView {
        points: &points,
        normals: &normals,
        columns: usize::from(grid.segments) + 1,
        rings: usize::from(grid.rings),
    };
    let limit = target.top + SEAM_MARGIN;
    let (crease, crease_y, crease_phi) = g.crease(limit);
    let ripple = g.ripple(limit);
    let flange = flange(&points, &mesh.indices, target);
    let h = TIP_SCALE * target.half_width;
    let front_tip = tip_angle(&points, &mesh.indices, |p| DVec2::new(p.x, p.y), h);
    let side_tip = tip_angle(&points, &mesh.indices, |p| DVec2::new(p.z, p.y), h);
    let lowest = points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    Quality {
        crease,
        flange,
        spike: front_tip.min(side_tip),
        ripple,
        front_tip,
        side_tip,
        crease_y,
        crease_phi,
        lowest,
    }
}

// ------------------------------------------------------------------------------------------------ crease and ripple

/// The head grid: `rings + 1` rows from the top pole, `columns` per row (the last repeats the first).
struct GridView<'a> {
    points: &'a [DVec3],
    normals: &'a [DVec3],
    columns: usize,
    rings: usize,
}

impl GridView<'_> {
    fn at(&self, ring: usize, column: usize) -> usize {
        ring * self.columns + column
    }

    /// Distinct columns (the seam column repeats the first).
    fn segments(&self) -> usize {
        self.columns - 1
    }

    /// The signed angle the polyline `a -> b -> c` turns through at `b` in the plane of its tangent and the normal at
    /// `b`, radians; positive where the surface is convex.
    fn turn(&self, a: usize, b: usize, c: usize) -> f64 {
        let (pa, pb, pc, normal) = (self.points[a], self.points[b], self.points[c], self.normals[b]);
        let (t1, t2) = ((pb - pa).normalize_or_zero(), (pc - pb).normalize_or_zero());
        let along = ((t1 + t2) - normal * (t1 + t2).dot(normal)).normalize_or_zero();
        let (u1, u2) = (DVec2::new(t1.dot(along), t1.dot(normal)), DVec2::new(t2.dot(along), t2.dot(normal)));
        -(u1.perp_dot(u2)).atan2(u1.dot(u2))
    }

    /// Whether vertex `(ring, column)` is in the measured region: in the lower face and clear of the bottom pole.
    fn measured(&self, ring: usize, column: usize, limit: f64) -> bool {
        ring >= 1 && ring + 2 <= self.rings && self.points[self.at(ring, column)].y <= limit
    }

    /// The largest jump in turning angle between neighbouring vertices along columns and rings, degrees, and where.
    fn crease(&self, limit: f64) -> (f64, f64, f64) {
        let mut worst = (0.0, 0.0, 0.0);
        let mut consider = |jump: f64, idx: usize, column: usize| {
            if jump > worst.0 {
                worst = (jump, self.points[idx].y, 360.0 * fraction(column, self.segments()));
            }
        };
        let segments = self.segments();
        for column in 0..segments {
            // Down the column: turning at ring r against ring r - 1.
            for ring in 2..self.rings - 1 {
                if !(self.measured(ring, column, limit) && self.measured(ring - 1, column, limit)) {
                    continue;
                }
                let turn = |r: usize| self.turn(self.at(r - 1, column), self.at(r, column), self.at(r + 1, column));
                consider((turn(ring) - turn(ring - 1)).abs().to_degrees(), self.at(ring, column), column);
            }
        }
        for ring in 1..self.rings - 1 {
            let turn = |c: usize| {
                let (prev, next) = ((c + segments - 1) % segments, (c + 1) % segments);
                self.turn(self.at(ring, prev), self.at(ring, c), self.at(ring, next))
            };
            for column in 0..segments {
                let prev = (column + segments - 1) % segments;
                if !(self.measured(ring, column, limit) && self.measured(ring, prev, limit)) {
                    continue;
                }
                consider((turn(column) - turn(prev)).abs().to_degrees(), self.at(ring, column), column);
            }
        }
        worst
    }

    /// The worst column's root-mean-square angle between a vertex normal and the mean of the normals above and below
    /// it, over the lower face, degrees.
    fn ripple(&self, limit: f64) -> f64 {
        let mut worst = 0.0_f64;
        for column in 0..self.segments() {
            let (mut sum, mut count) = (0.0, 0.0);
            for ring in 2..self.rings - 1 {
                if !self.measured(ring, column, limit) {
                    continue;
                }
                let mean = (self.normals[self.at(ring - 1, column)] + self.normals[self.at(ring + 1, column)])
                    .normalize_or_zero();
                let n = self.normals[self.at(ring, column)];
                let angle = n.cross(mean).length().atan2(n.dot(mean));
                sum += angle * angle;
                count += 1.0;
            }
            if count > 0.0 {
                worst = worst.max((sum / count).sqrt().to_degrees());
            }
        }
        worst
    }
}

// ------------------------------------------------------------------------------------------------ flange

/// The Hausdorff distance between the front silhouette's lower part (at or below the target's top) and the target
/// outline, the worse of the two sides, millimetres.
fn flange(points: &[DVec3], indices: &[u32], target: &Target) -> f64 {
    let wanted = target.points();
    [1.0, -1.0]
        .into_iter()
        .map(|side| {
            let outline = lower_silhouette(points, indices, side, target.top);
            hausdorff(&outline, &wanted) * 1e3
        })
        .fold(0.0, f64::max)
}

/// The front silhouette on one side (`side` 1 for +x, -1 for -x, mirrored to +x), at and below `top`: the outermost
/// point of every height slice, and the lowest point of every width slice (the underside of the chin).
fn lower_silhouette(points: &[DVec3], indices: &[u32], side: f64, top: f64) -> Vec<DVec2> {
    let flat: Vec<DVec2> = points.iter().map(|p| DVec2::new(p.x * side, p.y)).collect();
    let lowest = flat.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let widest = flat.iter().filter(|p| p.y <= top).map(|p| p.x).fold(0.0, f64::max);
    let outermost = slice_extremes(&flat, indices, |p| p.y, |p| p.x, lowest, top, f64::max);
    let underside = slice_extremes(&flat, indices, |p| p.x, |p| p.y, 0.0, widest, f64::min);
    let mut out: Vec<DVec2> = outermost.into_iter().filter_map(|(y, x)| x.map(|x| DVec2::new(x, y))).collect();
    out.extend(underside.into_iter().filter_map(|(x, y)| y.filter(|y| *y <= top).map(|y| DVec2::new(x, y))));
    out
}

/// For slices of `along` from `from` to `to` every [`SLICE`], the extreme (by `pick`) of `value` where the mesh's edges
/// cross the slice, or `None` where none do.
fn slice_extremes(
    flat: &[DVec2],
    indices: &[u32],
    along: impl Fn(DVec2) -> f64,
    value: impl Fn(DVec2) -> f64,
    from: f64,
    to: f64,
    pick: fn(f64, f64) -> f64,
) -> Vec<(f64, Option<f64>)> {
    let count = steps(to - from, SLICE) + 1;
    let mut best: Vec<Option<f64>> = vec![None; count];
    for tri in indices.as_chunks::<3>().0 {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            let (pa, pb) = (flat[a as usize], flat[b as usize]);
            let (sa, sb) = (along(pa), along(pb));
            let (lo, hi) = (sa.min(sb), sa.max(sb));
            let first = ((lo - from) / SLICE).ceil().max(0.0);
            let last = ((hi - from) / SLICE).floor().min((count - 1) as f64);
            let mut k = first;
            while k <= last {
                let s = from + k * SLICE;
                let t = if (sb - sa).abs() < 1e-15 { 0.0 } else { (s - sa) / (sb - sa) };
                let v = value(pa + (pb - pa) * t);
                let slot = &mut best[k as usize];
                *slot = Some(slot.map_or(v, |old| pick(old, v)));
                k += 1.0;
            }
        }
    }
    best.into_iter().enumerate().map(|(k, v)| (from + k as f64 * SLICE, v)).collect()
}

/// The symmetric Hausdorff distance between two dense point sets standing in for curves.
fn hausdorff(a: &[DVec2], b: &[DVec2]) -> f64 {
    let one_way = |from: &[DVec2], to: &[DVec2]| {
        from.iter()
            .map(|p| to.iter().map(|q| p.distance_squared(*q)).fold(f64::INFINITY, f64::min))
            .fold(0.0, f64::max)
            .sqrt()
    };
    if a.is_empty() || b.is_empty() {
        return f64::INFINITY;
    }
    one_way(a, b).max(one_way(b, a))
}

// ------------------------------------------------------------------------------------------------ spike

/// The silhouette's angle at its lowest point in the view `project` (a horizontal view: the image's y is height), at
/// radius `h`: 360 degrees minus the empty arc below the tip on the circle of radius `h` round it. The silhouette is
/// rasterised near the tip only.
fn tip_angle(points: &[DVec3], indices: &[u32], project: impl Fn(DVec3) -> DVec2, h: f64) -> f64 {
    let flat: Vec<DVec2> = points.iter().map(|p| project(*p)).collect();
    let Some(tip) = flat.iter().copied().min_by(|left, right| left.y.total_cmp(&right.y)) else { return 0.0 };
    let reach = h * 1.05;
    let side = steps(2.0 * reach, TIP_PIXEL) + 1;
    let origin = tip - DVec2::splat(reach);
    let mut filled = vec![false; side * side];
    for tri in indices.as_chunks::<3>().0 {
        let [p0, p1, p2] = tri.map(|i| flat[i as usize]);
        let (lo, hi) = (p0.min(p1).min(p2), p0.max(p1).max(p2));
        if hi.x < origin.x || hi.y < origin.y || lo.x > origin.x + 2.0 * reach || lo.y > origin.y + 2.0 * reach {
            continue;
        }
        let area = (p1 - p0).perp_dot(p2 - p0);
        if area.abs() < 1e-16 {
            continue;
        }
        let px = |v: f64, o: f64| ((v - o) / TIP_PIXEL).floor().clamp(0.0, (side - 1) as f64) as usize;
        for iy in px(lo.y, origin.y)..=px(hi.y, origin.y) {
            for ix in px(lo.x, origin.x)..=px(hi.x, origin.x) {
                let pixel = origin + DVec2::new((ix as f64 + 0.5) * TIP_PIXEL, (iy as f64 + 0.5) * TIP_PIXEL);
                let w0 = (p2 - p1).perp_dot(pixel - p1) / area;
                let w1 = (p0 - p2).perp_dot(pixel - p2) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    filled[iy * side + ix] = true;
                }
            }
        }
    }
    let inside = |angle: f64| {
        let at = tip + DVec2::new(angle.cos(), angle.sin()) * h - origin;
        let (ix, iy) = ((at.x / TIP_PIXEL).floor(), (at.y / TIP_PIXEL).floor());
        ix >= 0.0
            && iy >= 0.0
            && (ix as usize) < side
            && (iy as usize) < side
            && filled[iy as usize * side + ix as usize]
    };
    // Walk the circle both ways from straight below the tip (always empty) to the first filled pixel.
    let step = 0.1_f64.to_radians();
    let down = -PI / 2.0;
    let arc = |dir: f64| {
        let mut swept = 0.0;
        while swept < PI && !inside(down + dir * swept) {
            swept += step;
        }
        swept
    };
    (360.0 - (arc(1.0) + arc(-1.0)).to_degrees()).max(0.0)
}

// ------------------------------------------------------------------------------------------------ helpers

/// Whole steps of `step` in `length` (at least 1).
fn steps(length: f64, step: f64) -> usize {
    ((length / step).ceil() as usize).max(1)
}

fn fraction(k: usize, n: usize) -> f64 {
    k as f64 / n as f64
}
