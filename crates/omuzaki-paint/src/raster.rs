//! Coverage of filled paths, and the mask operations shapes and fills need: feathering (a Gaussian blur) and growing
//! or shrinking a region by whole pixels.
//!
//! A path is flattened to lines (kurbo, to a fiftieth of a pixel) and swept a pixel row at a time with
//! [`SUBROWS`] sub-scanlines; along each sub-scanline the spans inside the path add their exact horizontal coverage.
//! That anti-aliases edges to 1/16 of a pixel vertically and exactly horizontally, and honours both fill rules, which
//! an area-accumulation rasteriser cannot.

use kurbo::{BezPath, PathEl, Shape as _};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::canvas::{Mask, Rect};

/// Sub-scanlines per pixel row.
pub const SUBROWS: usize = 16;
/// How far a flattened curve may stray from the true one, in pixels (a fiftieth: a circle loses under 0.1% of its area).
pub const FLATTEN_TOLERANCE: f64 = 0.02;

/// Which parts of a self-overlapping path are inside.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FillRule {
    /// Inside where the path winds round a point at all (overlapping parts stay filled).
    #[default]
    Nonzero,
    /// Inside where the path crosses an odd number of times (a ring's hole stays empty).
    Evenodd,
}

/// One line of a flattened path, top to bottom, with the way it was drawn.
#[derive(Copy, Clone, Debug)]
struct Edge {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    /// +1 drawn downwards, -1 upwards.
    dir: i32,
}

/// Flattens `path` into closed polygons of points.
pub fn polygons(path: &BezPath) -> Vec<Vec<(f64, f64)>> {
    let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<(f64, f64)> = Vec::new();
    kurbo::flatten(path, FLATTEN_TOLERANCE, |el| match el {
        PathEl::MoveTo(p) => {
            if current.len() > 1 {
                out.push(std::mem::take(&mut current));
            }
            current.clear();
            current.push((p.x, p.y));
        }
        PathEl::LineTo(p) => current.push((p.x, p.y)),
        PathEl::ClosePath => {
            if current.len() > 1 {
                out.push(std::mem::take(&mut current));
            }
            current.clear();
        }
        // `flatten` only emits moves, lines and closes.
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
    });
    if current.len() > 1 {
        out.push(current);
    }
    out
}

fn edges(polys: &[Vec<(f64, f64)>]) -> Vec<Edge> {
    let mut out = Vec::new();
    for poly in polys {
        for (i, &(ax, ay)) in poly.iter().enumerate() {
            let (bx, by) = poly[(i + 1) % poly.len()];
            if (ay - by).abs() < 1e-12 || !(ax.is_finite() && ay.is_finite() && bx.is_finite() && by.is_finite()) {
                continue;
            }
            if ay < by {
                out.push(Edge { x0: ax, y0: ay, x1: bx, y1: by, dir: 1 });
            } else {
                out.push(Edge { x0: bx, y0: by, x1: ax, y1: ay, dir: -1 });
            }
        }
    }
    out
}

/// Adds the coverage of the span `x0..x1` (fractional pixels) to `row`, which starts at pixel `left`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // row widths are at most 8192
fn add_span(row: &mut [f32], left: i32, x0: f64, x1: f64, weight: f32) {
    let w = row.len();
    let lo = (x0 - f64::from(left)).max(0.0);
    let hi = (x1 - f64::from(left)).min(w as f64);
    if hi <= lo {
        return;
    }
    let (i0, i1) = (lo.floor() as usize, hi.floor() as usize);
    if i0 == i1 {
        row[i0.min(w - 1)] += ((hi - lo) as f32) * weight;
        return;
    }
    row[i0] += ((i0 as f64 + 1.0 - lo) as f32) * weight;
    for v in &mut row[i0 + 1..i1.min(w)] {
        *v += weight;
    }
    if i1 < w {
        row[i1] += ((hi - i1 as f64) as f32) * weight;
    }
}

/// The coverage of `path` filled with `rule`, inside `clip` (the canvas).
pub fn fill_path(path: &BezPath, rule: FillRule, clip: Rect) -> Mask {
    let b = path.bounding_box();
    let rect = Rect::covering(b.x0, b.y0, b.x1, b.y1).clip(clip);
    fill_polygons(&polygons(path), rule, rect)
}

/// The coverage of closed `polys` filled with `rule`, over `rect`.
#[allow(clippy::cast_precision_loss)] // SUBROWS is small
pub fn fill_polygons(polys: &[Vec<(f64, f64)>], rule: FillRule, rect: Rect) -> Mask {
    let mut mask = Mask::new(rect);
    if rect.is_empty() {
        return mask;
    }
    let mut all = edges(polys);
    all.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    let width = rect.width();
    let weight = 1.0 / SUBROWS as f32;
    let mut next = 0;
    let mut active: Vec<Edge> = Vec::new();
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for (row_i, py) in (rect.y0..rect.y1).enumerate() {
        let (top, bottom) = (f64::from(py), f64::from(py) + 1.0);
        active.retain(|e| e.y1 > top);
        while next < all.len() && all[next].y0 < bottom {
            if all[next].y1 > top {
                active.push(all[next]);
            }
            next += 1;
        }
        if active.is_empty() {
            continue;
        }
        let row = &mut mask.values[row_i * width..(row_i + 1) * width];
        for s in 0..SUBROWS {
            let y = top + (s as f64 + 0.5) / SUBROWS as f64;
            crossings.clear();
            for e in &active {
                if e.y0 <= y && y < e.y1 {
                    let t = (y - e.y0) / (e.y1 - e.y0);
                    crossings.push((e.x0 + (e.x1 - e.x0) * t, e.dir));
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            for pair in crossings.windows(2) {
                wind += pair[0].1;
                let inside = match rule {
                    FillRule::Nonzero => wind != 0,
                    FillRule::Evenodd => wind % 2 != 0,
                };
                if inside {
                    add_span(row, rect.x0, pair[0].0, pair[1].0, weight);
                }
            }
        }
    }
    for v in &mut mask.values {
        *v = v.clamp(0.0, 1.0);
    }
    mask
}

/// Box widths whose three passes approximate a Gaussian of `sigma` pixels (Kovesi's method).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small, positive radii
fn box_radii(sigma: f64) -> [usize; 3] {
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut lo = ideal.floor() as i64;
    if lo % 2 == 0 {
        lo -= 1;
    }
    let lo = lo.max(1);
    let hi = lo + 2;
    let lo_f = lo as f64;
    let m = ((12.0 * sigma * sigma - n * lo_f * lo_f - 4.0 * n * lo_f - 3.0 * n) / (-4.0 * lo_f - 4.0)).round();
    let mut out = [0; 3];
    for (i, r) in out.iter_mut().enumerate() {
        let size = if (i as f64) < m { lo } else { hi };
        *r = ((size - 1) / 2) as usize;
    }
    out
}

fn box_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    #[allow(clippy::cast_precision_loss)] // small radii
    let inv = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let mut acc = 0.0f32;
        // Pixels past the edge count as 0, so a feathered edge fades out instead of smearing.
        for i in 0..=r.min(len.saturating_sub(1)) {
            acc += src[at(line, i)];
        }
        for i in 0..len {
            dst[at(line, i)] = acc * inv;
            if i + r + 1 < len {
                acc += src[at(line, i + r + 1)];
            }
            if i >= r {
                acc -= src[at(line, i - r)];
            }
        }
    }
}

/// Blurs `mask` by a Gaussian of `sigma` pixels (feathering), growing it so nothing is cut off, inside `clip`.
#[allow(clippy::cast_possible_truncation)] // sigma is capped by the model's limits
pub fn feather(mask: &Mask, sigma: f64, clip: Rect) -> Mask {
    if sigma <= 0.0 || mask.rect.is_empty() {
        return mask.clone();
    }
    let pad = (sigma * 3.0).ceil() as i32 + 1;
    let rect = mask.rect.grow(pad).clip(clip);
    let mut a = Mask::new(rect);
    for y in mask.rect.y0..mask.rect.y1 {
        for x in mask.rect.x0..mask.rect.x1 {
            if let Some(v) = a.at_mut(x, y) {
                *v = mask.get(x, y);
            }
        }
    }
    let (w, h) = (rect.width(), rect.height());
    let mut b = vec![0.0; a.values.len()];
    for r in box_radii(sigma) {
        box_pass(&a.values, &mut b, w, h, r, true);
        box_pass(&b, &mut a.values, w, h, r, false);
    }
    a
}

/// Grows (`by > 0`) or shrinks (`by < 0`) the region where `mask` is at least one half, by whole pixels (a square
/// neighbourhood, which is what a painter's "expand" and "close gap" do).
pub fn morph(mask: &Mask, by: i32, clip: Rect) -> Mask {
    if by == 0 {
        return mask.clone();
    }
    let n = by.abs();
    let rect = if by > 0 { mask.rect.grow(n).clip(clip) } else { mask.rect };
    let inside = |x: i32, y: i32| mask.get(x, y) >= 0.5;
    let mut out = Mask::new(rect);
    // Separable: a square max (or min) is a row max of a column max.
    let (w, h) = (rect.width(), rect.height());
    let mut cols = vec![false; w * h];
    for (yi, y) in (rect.y0..rect.y1).enumerate() {
        for (xi, x) in (rect.x0..rect.x1).enumerate() {
            let hit = (-n..=n).map(|d| inside(x, y + d));
            cols[yi * w + xi] = if by > 0 { hit.into_iter().any(|v| v) } else { hit.into_iter().all(|v| v) };
        }
    }
    for yi in 0..h {
        for xi in 0..w {
            let lo = xi.saturating_sub(n.unsigned_abs() as usize);
            let hi = (xi + n.unsigned_abs() as usize).min(w - 1);
            let window = cols[yi * w + lo..=yi * w + hi].iter();
            let on = if by > 0 {
                window.into_iter().any(|&v| v)
            } else {
                // A pixel whose square reaches past the mask's own rectangle is next to outside.
                let edge = xi < n.unsigned_abs() as usize || xi + n.unsigned_abs() as usize >= w;
                !edge && window.into_iter().all(|&v| v)
            };
            out.values[yi * w + xi] = if on { 1.0 } else { 0.0 };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use kurbo::{Circle, Rect as KRect, Shape};

    use super::*;

    const CLIP: Rect = Rect { x0: 0, y0: 0, x1: 200, y1: 200 };

    #[test]
    fn a_square_on_whole_pixels_is_exact() {
        let m = fill_path(&KRect::new(10.0, 10.0, 20.0, 30.0).to_path(0.1), FillRule::Nonzero, CLIP);
        assert!((m.area() - 200.0).abs() < 1e-3, "{}", m.area());
        assert!((m.get(10, 10) - 1.0).abs() < 1e-6 && m.get(9, 10) == 0.0 && m.get(20, 10) == 0.0);
    }

    #[test]
    fn a_half_pixel_edge_is_half_covered() {
        let m = fill_path(&KRect::new(10.5, 10.0, 20.0, 20.0).to_path(0.1), FillRule::Nonzero, CLIP);
        assert!((m.get(10, 15) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn a_circle_covers_pi_r_squared() {
        let m = fill_path(&Circle::new((100.0, 100.0), 40.0).to_path(0.01), FillRule::Nonzero, CLIP);
        let want = std::f64::consts::PI * 1600.0;
        assert!((m.area() - want).abs() / want < 2e-3, "{} vs {want}", m.area());
    }

    #[test]
    fn even_odd_leaves_the_hole_of_a_ring() {
        let mut p = Circle::new((100.0, 100.0), 40.0).to_path(0.01);
        p.extend(Circle::new((100.0, 100.0), 20.0).to_path(0.01));
        let ring = fill_path(&p, FillRule::Evenodd, CLIP);
        let solid = fill_path(&p, FillRule::Nonzero, CLIP);
        assert_eq!(ring.get(100, 100), 0.0);
        assert!((solid.get(100, 100) - 1.0).abs() < 1e-6);
        assert!((ring.area() - std::f64::consts::PI * 1200.0).abs() < 10.0);
    }

    #[test]
    fn feathering_keeps_the_area_and_softens_the_edge() {
        let m = fill_path(&KRect::new(50.0, 50.0, 100.0, 100.0).to_path(0.1), FillRule::Nonzero, CLIP);
        let f = feather(&m, 4.0, CLIP);
        assert!((f.area() - m.area()).abs() / m.area() < 1e-3);
        let edge = f.get(50, 75);
        assert!(edge > 0.3 && edge < 0.7, "{edge}");
        assert!(f.get(75, 75) > 0.99);
    }

    #[test]
    fn morph_grows_and_shrinks_by_whole_pixels() {
        let m = fill_path(&KRect::new(50.0, 50.0, 60.0, 60.0).to_path(0.1), FillRule::Nonzero, CLIP);
        assert!((morph(&m, 2, CLIP).area() - 196.0).abs() < 1e-6);
        assert!((morph(&m, -2, CLIP).area() - 36.0).abs() < 1e-6);
    }
}
