//! Hatching: parallel lines across a region, each cut to the region's edge and thinned in at both ends (the blush
//! lines on an anime cheek, shading hatches, fabric texture).

use crate::noise::Rng;

/// How a region is hatched.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HatchLines {
    /// Direction, degrees clockwise from pointing right on screen.
    pub angle: f64,
    /// Gap between lines, pixels.
    pub spacing: f64,
    /// Share of each line that thins in at each end, 0 to 0.5.
    pub taper: f64,
    /// Share of each line cut off at each end, 0 to 0.45.
    pub inset: f64,
    /// Random wobble, 0 to 1.
    pub jitter: f64,
    /// The wobble's seed.
    pub seed: u64,
}

/// The most lines one hatch draws.
pub const MAX_LINES: usize = 20_000;

/// The lines across `polys` (closed polygons, inside by even-odd), each as `[x, y, pressure]` points whose pressure
/// draws the tapers.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // bounded counts
pub fn hatch_lines(polys: &[Vec<(f64, f64)>], o: HatchLines) -> Vec<Vec<[f64; 3]>> {
    let (dir_x, dir_y) = (libm::cos(o.angle.to_radians()), libm::sin(o.angle.to_radians()));
    let (nx, ny) = (-dir_y, dir_x);
    // The region's extent across the lines (along the normal).
    let across = polys.iter().flatten().map(|&(x, y)| x * nx + y * ny);
    let (lo, hi) = across.fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| (l.min(v), h.max(v)));
    if !lo.is_finite() || hi <= lo {
        return Vec::new();
    }
    let mut rng = Rng::new(o.seed);
    // Lines sit inside the region, centred, never on its edge.
    let count = (((hi - lo) / o.spacing).floor() as usize).clamp(1, MAX_LINES);
    let offset = (hi - lo - (count - 1) as f64 * o.spacing) / 2.0;
    let mut out = Vec::new();
    for i in 0..count {
        let c = lo + offset + i as f64 * o.spacing + rng.signed() * o.jitter * o.spacing * 0.3;
        // Where the line `p . n = c` crosses each edge, as distances along the line's direction.
        let mut ts: Vec<f64> = Vec::new();
        for poly in polys {
            for (k, &(ax, ay)) in poly.iter().enumerate() {
                let (bx, by) = poly[(k + 1) % poly.len()];
                let (da, db) = (ax * nx + ay * ny - c, bx * nx + by * ny - c);
                if (da < 0.0) != (db < 0.0) {
                    let t = da / (da - db);
                    let (x, y) = (ax + (bx - ax) * t, ay + (by - ay) * t);
                    ts.push(x * dir_x + y * dir_y);
                }
            }
        }
        ts.sort_by(f64::total_cmp);
        for pair in ts.as_chunks::<2>().0 {
            let (mut a, mut b) = (pair[0], pair[1]);
            let len = b - a;
            a += len * (o.inset + rng.next() * o.jitter * 0.15);
            b -= len * (o.inset + rng.next() * o.jitter * 0.15);
            if b - a < 0.5 {
                continue;
            }
            let at = |t: f64| (c * nx + t * dir_x, c * ny + t * dir_y);
            let steps = 8;
            let mut line = Vec::with_capacity(steps + 1);
            for s in 0..=steps {
                let u = s as f64 / steps as f64;
                let edge = u.min(1.0 - u);
                let p = if o.taper > 0.0 { (edge / o.taper).min(1.0) } else { 1.0 };
                let (x, y) = at(a + (b - a) * u);
                line.push([x, y, p.max(0.05)]);
            }
            out.push(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Vec<Vec<(f64, f64)>> {
        vec![vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)]]
    }

    #[test]
    fn horizontal_hatching_spans_the_square() {
        let o = HatchLines { angle: 0.0, spacing: 10.0, taper: 0.3, inset: 0.0, jitter: 0.0, seed: 0 };
        let lines = hatch_lines(&square(), o);
        assert_eq!(lines.len(), 10);
        for l in &lines {
            let (first, last) = (l[0], l[l.len() - 1]);
            assert!(first[0].abs() < 1e-6 && (last[0] - 100.0).abs() < 1e-6);
            assert!(first[2] < 0.1 && l[4][2] > 0.99);
        }
    }

    #[test]
    fn inset_shortens_and_diagonals_stay_inside() {
        let o = HatchLines { angle: 45.0, spacing: 7.0, taper: 0.0, inset: 0.2, jitter: 0.5, seed: 3 };
        for l in hatch_lines(&square(), o) {
            for p in l {
                assert!((-1e-6..=100.0 + 1e-6).contains(&p[0]) && (-1e-6..=100.0 + 1e-6).contains(&p[1]));
            }
        }
    }
}
