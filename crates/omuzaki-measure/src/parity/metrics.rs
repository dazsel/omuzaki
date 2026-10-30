//! The per-pixel measures of a render against a reference on one pixel grid: the exact difference map (the stop
//! rule's map), CIEDE2000, SSIM, gradient-magnitude similarity, edges and their distance, and silhouettes; plus the
//! statistics of a map over a mask.

use super::raster::{LabImage, Plane};

/// The CIEDE2000 colour difference of two CIELAB colours (Sharma, Wu and Dalal 2005). About 1 is the smallest
/// difference a viewer can see side by side; 2.3 is the usual "just noticeable" in a scene.
#[must_use]
#[allow(clippy::many_single_char_names, clippy::similar_names)] // the published formula's names
pub fn ciede2000(p: [f64; 3], q: [f64; 3]) -> f64 {
    const POW25_7: f64 = 6_103_515_625.0; // 25^7
    let [l1, a1, b1] = p;
    let [l2, a2, b2] = q;
    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let cbar7 = f64::midpoint(c1, c2).powi(7);
    let g = 0.5 * (1.0 - (cbar7 / (cbar7 + POW25_7)).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = (a1p.hypot(b1), a2p.hypot(b2));
    let hue = |b: f64, a: f64| if a == 0.0 && b == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let product = c1p * c2p;
    let dh_deg = if product == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d.abs() <= 180.0 {
            d
        } else if d > 180.0 {
            d - 360.0
        } else {
            d + 360.0
        }
    };
    let dh = 2.0 * product.sqrt() * (dh_deg / 2.0).to_radians().sin();
    let lbar = f64::midpoint(l1, l2);
    let cbarp = f64::midpoint(c1p, c2p);
    let hbar = if product == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        f64::midpoint(h1p, h2p)
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let cos = |deg: f64| deg.to_radians().cos();
    let t = 1.0 - 0.17 * cos(hbar - 30.0) + 0.24 * cos(2.0 * hbar) + 0.32 * cos(3.0 * hbar + 6.0)
        - 0.20 * cos(4.0 * hbar - 63.0);
    let dtheta = 30.0 * (-((hbar - 275.0) / 25.0).powi(2)).exp();
    let cbarp7 = cbarp.powi(7);
    let rc = 2.0 * (cbarp7 / (cbarp7 + POW25_7)).sqrt();
    let sl = 1.0 + 0.015 * (lbar - 50.0).powi(2) / (20.0 + (lbar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cbarp;
    let sh = 1.0 + 0.015 * cbarp * t;
    let rt = -(2.0 * dtheta).to_radians().sin() * rc;
    let (x, y, z) = (dl / sl, dc / sc, dh / sh);
    (x * x + y * y + z * z + rt * y * z).max(0.0).sqrt()
}

/// The CIEDE2000 map of two images of one size.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // a colour difference fits an f32
pub fn delta_e_map(ours: &LabImage, reference: &LabImage) -> Plane {
    let v = (0..ours.w * ours.h)
        .map(|i| {
            let p = ours.lab_at(i).map(f64::from);
            let q = reference.lab_at(i).map(f64::from);
            ciede2000(p, q) as f32
        })
        .collect();
    Plane { w: ours.w, h: ours.h, v }
}

/// The exact difference map: per pixel, the largest of the three sRGB byte differences (0 to 255). The stop rule's
/// map: parity is reached when this is 0 everywhere that is scored.
#[must_use]
pub fn level_map(ours: &LabImage, reference: &LabImage) -> Plane {
    let v = ours
        .rgb
        .iter()
        .zip(&reference.rgb)
        .map(|(p, q)| f32::from((0..3).map(|k| p[k].abs_diff(q[k])).max().unwrap_or(0)))
        .collect();
    Plane { w: ours.w, h: ours.h, v }
}

/// The SSIM map on L* (Wang et al. 2004, Gaussian windows of sigma 1.5): 1 where the local structure, contrast and
/// brightness agree.
#[must_use]
pub fn ssim_map(x: &Plane, y: &Plane) -> Plane {
    const C1: f32 = 1.0; // (0.01 * 100)^2: L* runs 0 to 100
    const C2: f32 = 9.0; // (0.03 * 100)^2
    let sigma = 1.5;
    let mx = x.blur(sigma);
    let my = y.blur(sigma);
    let xx = x.zip(x, |a, b| a * b).blur(sigma);
    let yy = y.zip(y, |a, b| a * b).blur(sigma);
    let xy = x.zip(y, |a, b| a * b).blur(sigma);
    let v = (0..x.v.len())
        .map(|i| {
            let (ux, uy) = (mx.v[i], my.v[i]);
            // Not clamped at 0: equal planes then give exactly equal variances and covariance, so exactly 1.
            let vx = xx.v[i] - ux * ux;
            let vy = yy.v[i] - uy * uy;
            let cov = xy.v[i] - ux * uy;
            ((2.0 * ux * uy + C1) * (2.0 * cov + C2)) / ((ux * ux + uy * uy + C1) * (vx + vy + C2))
        })
        .collect();
    Plane { w: x.w, h: x.h, v }
}

/// The Sobel gradient of a plane, in units per pixel: `(gx, gy)`.
#[must_use]
#[allow(clippy::cast_possible_wrap)] // image sides are far below isize::MAX
pub fn sobel(p: &Plane) -> (Plane, Plane) {
    let mut gx = Plane::new(p.w, p.h, 0.0);
    let mut gy = Plane::new(p.w, p.h, 0.0);
    for y in 0..p.h {
        for x in 0..p.w {
            let (xi, yi) = (x as isize, y as isize);
            let at = |dx: isize, dy: isize| p.at(xi + dx, yi + dy);
            let sx = (at(1, -1) + 2.0 * at(1, 0) + at(1, 1)) - (at(-1, -1) + 2.0 * at(-1, 0) + at(-1, 1));
            let sy = (at(-1, 1) + 2.0 * at(0, 1) + at(1, 1)) - (at(-1, -1) + 2.0 * at(0, -1) + at(1, -1));
            gx.v[y * p.w + x] = sx / 8.0;
            gy.v[y * p.w + x] = sy / 8.0;
        }
    }
    (gx, gy)
}

/// The gradient-magnitude similarity map (Xue et al. 2014, GMSD), on L*: 1 where the edges have the same strength.
#[must_use]
pub fn gms_map(x: &Plane, y: &Plane) -> Plane {
    const C: f32 = 0.26; // the paper's 170 on 0..255 Sobel/4, rescaled to L* per pixel
    let (ax, ay) = sobel(x);
    let (bx, by) = sobel(y);
    let ma = ax.zip(&ay, f32::hypot);
    let mb = bx.zip(&by, f32::hypot);
    ma.zip(&mb, |a, b| (2.0 * a * b + C) / (a * a + b * b + C))
}

/// The L* gradient (per pixel) above which a pixel can be an edge.
pub const EDGE_LSTAR: f32 = 3.0;

/// The thin edges of a plane: pixels whose Sobel magnitude passes [`EDGE_LSTAR`] and is a local maximum across the
/// gradient (non-maximum suppression in four directions).
#[must_use]
#[allow(clippy::cast_possible_wrap)] // image sides are far below isize::MAX
pub fn edges(p: &Plane) -> Vec<bool> {
    let (gx, gy) = sobel(p);
    let mag = gx.zip(&gy, f32::hypot);
    let mut out = vec![false; p.w * p.h];
    for y in 0..p.h {
        for x in 0..p.w {
            let i = y * p.w + x;
            let m = mag.v[i];
            if m < EDGE_LSTAR {
                continue;
            }
            let angle = gy.v[i].atan2(gx.v[i]).to_degrees().rem_euclid(180.0);
            let (dx, dy) = if !(22.5..157.5).contains(&angle) {
                (1, 0)
            } else if angle < 67.5 {
                (1, 1)
            } else if angle < 112.5 {
                (0, 1)
            } else {
                (-1, 1)
            };
            let (xi, yi) = (x as isize, y as isize);
            if m >= mag.at(xi + dx, yi + dy) && m > mag.at(xi - dx, yi - dy) {
                out[i] = true;
            }
        }
    }
    out
}

/// The exact Euclidean distance (pixels) from every pixel to the nearest `true` one (Felzenszwalb and Huttenlocher
/// 2012). With no `true` pixel every distance is `f32::INFINITY`.
#[must_use]
#[allow(clippy::cast_precision_loss)] // pixel indices
pub fn distance(w: usize, h: usize, set: &[bool]) -> Plane {
    const BIG: f64 = 1e20;
    let mut f: Vec<f64> = set.iter().map(|&s| if s { 0.0 } else { BIG }).collect();
    let mut column = vec![0.0; h];
    for x in 0..w {
        for y in 0..h {
            column[y] = f[y * w + x];
        }
        let d = edt_1d(&column);
        for y in 0..h {
            f[y * w + x] = d[y];
        }
    }
    let mut row = vec![0.0; w];
    for y in 0..h {
        row.copy_from_slice(&f[y * w..(y + 1) * w]);
        let d = edt_1d(&row);
        f[y * w..(y + 1) * w].copy_from_slice(&d);
    }
    #[allow(clippy::cast_possible_truncation)] // distances within an image fit an f32
    let v = f.iter().map(|&d| if d >= BIG / 2.0 { f32::INFINITY } else { d.sqrt() as f32 }).collect();
    Plane { w, h, v }
}

/// The squared distance transform of a sampled function in one dimension (the lower envelope of parabolas).
#[allow(clippy::cast_precision_loss)] // indices
fn edt_1d(f: &[f64]) -> Vec<f64> {
    let n = f.len();
    let mut d = vec![0.0; n];
    if n == 0 {
        return d;
    }
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f64; n + 1];
    let mut k = 0usize;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        // z[0] is minus infinity, so the walk back stops at the first parabola at the latest.
        let mut s;
        loop {
            let p = v[k];
            s = ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * q as f64 - 2.0 * p as f64);
            if s <= z[k] && k > 0 {
                k -= 1;
            } else {
                break;
            }
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f64 - p as f64;
        *out = dq * dq + f[p];
    }
    d
}

/// The silhouette of an image: its alpha above one half when it has alpha, else every pixel further than
/// `key_delta_e` (CIE76) from the `backdrop` colour.
#[must_use]
pub fn silhouette(image: &LabImage, backdrop: [f32; 3], key_delta_e: f32) -> Vec<bool> {
    if image.has_alpha {
        return image.alpha.v.iter().map(|&a| a > 0.5).collect();
    }
    (0..image.w * image.h)
        .map(|i| {
            let c = image.lab_at(i);
            let d = (c[0] - backdrop[0]).powi(2) + (c[1] - backdrop[1]).powi(2) + (c[2] - backdrop[2]).powi(2);
            d.sqrt() > key_delta_e
        })
        .collect()
}

/// The median colour of an image's one-pixel border, in CIELAB: its backdrop when it has one.
#[must_use]
pub fn border_colour(image: &LabImage) -> [f32; 3] {
    let (w, h) = (image.w, image.h);
    let mut idx: Vec<usize> = (0..w).flat_map(|x| [x, (h - 1) * w + x]).collect();
    idx.extend((0..h).flat_map(|y| [y * w, y * w + w - 1]));
    let median = |plane: &Plane| {
        let mut v: Vec<f32> = idx.iter().map(|&i| plane.v[i]).collect();
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    [median(&image.l), median(&image.a), median(&image.b)]
}

/// Statistics of a map over the pixels a mask keeps.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// How many pixels were kept.
    pub count: usize,
    /// The mean.
    pub mean: f64,
    /// The median.
    pub p50: f64,
    /// The 95th percentile.
    pub p95: f64,
    /// The largest value.
    pub max: f64,
    /// The sum.
    pub sum: f64,
}

/// The statistics of `map` over the pixels `keep` marks (the whole map when `keep` is `None`). Non-finite values are
/// left out.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // counts and ranks
pub fn stats(map: &Plane, keep: Option<&[bool]>) -> Stats {
    let mut v: Vec<f32> = match keep {
        Some(k) => map.v.iter().zip(k).filter(|(_, k)| **k).map(|(v, _)| *v).filter(|v| v.is_finite()).collect(),
        None => map.v.iter().copied().filter(|v| v.is_finite()).collect(),
    };
    if v.is_empty() {
        return Stats::default();
    }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    let sum: f64 = v.iter().map(|x| f64::from(*x)).sum();
    let rank = |q: f64| f64::from(v[((q * (n - 1) as f64).round() as usize).min(n - 1)]);
    Stats { count: n, mean: sum / n as f64, p50: rank(0.5), p95: rank(0.95), max: f64::from(v[n - 1]), sum }
}

/// The share of the kept pixels where `pred` holds (1 when nothing is kept).
#[must_use]
#[allow(clippy::cast_precision_loss)] // counts
pub fn share(map: &Plane, keep: Option<&[bool]>, pred: impl Fn(f32) -> bool) -> f64 {
    let (mut n, mut hit) = (0usize, 0usize);
    for (i, v) in map.v.iter().enumerate() {
        if keep.is_some_and(|k| !k[i]) {
            continue;
        }
        n += 1;
        hit += usize::from(pred(*v));
    }
    if n == 0 { 1.0 } else { hit as f64 / n as f64 }
}

/// The intersection over union of two masks, within `keep` (1 when both are empty).
#[must_use]
#[allow(clippy::cast_precision_loss)] // counts
pub fn iou(a: &[bool], b: &[bool], keep: Option<&[bool]>) -> f64 {
    let (mut inter, mut union) = (0usize, 0usize);
    for i in 0..a.len() {
        if keep.is_some_and(|k| !k[i]) {
            continue;
        }
        inter += usize::from(a[i] && b[i]);
        union += usize::from(a[i] || b[i]);
    }
    if union == 0 { 1.0 } else { inter as f64 / union as f64 }
}

/// The symmetric mean edge distance in pixels, within `keep`: the mean, over every edge pixel of each image, of the
/// distance to the nearest edge of the other, capped at `cap` pixels. `None` when neither image has an edge there.
#[must_use]
#[allow(clippy::cast_precision_loss)] // counts
pub fn edge_offset(
    ours: &[bool],
    reference: &[bool],
    w: usize,
    h: usize,
    keep: Option<&[bool]>,
    cap: f32,
) -> Option<f64> {
    let to_ref = distance(w, h, reference);
    let to_ours = distance(w, h, ours);
    let (mut sum, mut n) = (0.0f64, 0usize);
    for i in 0..w * h {
        if keep.is_some_and(|k| !k[i]) {
            continue;
        }
        if ours[i] {
            sum += f64::from(to_ref.v[i].min(cap));
            n += 1;
        }
        if reference[i] {
            sum += f64::from(to_ours.v[i].min(cap));
            n += 1;
        }
    }
    (n > 0).then(|| sum / n as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciede2000_matches_the_published_test_pairs() {
        // Sharma, Wu and Dalal (2005), table 1, pairs 1, 2, 7 and 17.
        let cases = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
        ];
        for (p, q, want) in cases {
            let got = ciede2000(p, q);
            assert!((got - want).abs() < 1e-4, "{p:?} {q:?}: {got} against {want}");
        }
        assert!(ciede2000([40.0, 10.0, -5.0], [40.0, 10.0, -5.0]).abs() < 1e-12);
    }

    #[test]
    fn the_distance_transform_is_euclidean() {
        let (w, h) = (7, 5);
        let mut set = vec![false; w * h];
        set[2 * w + 3] = true;
        let d = distance(w, h, &set);
        assert!(d.v[2 * w + 3].abs() < 1e-6);
        assert!((d.v[2 * w + 6] - 3.0).abs() < 1e-6);
        assert!((d.v[4 * w + 5] - 8f32.sqrt()).abs() < 1e-5);
        assert!(distance(3, 3, &[false; 9]).v.iter().all(|v| v.is_infinite()));
    }

    #[test]
    fn ssim_is_one_for_equal_planes_and_lower_for_noise() {
        let p = Plane { w: 16, h: 16, v: (0..256).map(|i| ((i * 37) % 100) as f32).collect() };
        assert!(ssim_map(&p, &p).v.iter().all(|s| (s - 1.0).abs() < 1e-4));
        let q = p.map(|v| 100.0 - v);
        assert!(stats(&ssim_map(&p, &q), None).mean < 0.5);
    }

    #[test]
    fn an_edge_one_pixel_off_measures_one_pixel() {
        let (w, h) = (20, 20);
        let step = |at: usize| Plane { w, h, v: (0..w * h).map(|i| if i % w < at { 10.0 } else { 80.0 }).collect() };
        let (a, b) = (edges(&step(9)), edges(&step(10)));
        let off = edge_offset(&a, &b, w, h, None, 50.0).unwrap();
        assert!((off - 1.0).abs() < 0.01, "{off}");
        assert!(edge_offset(&a, &a, w, h, None, 50.0).unwrap().abs() < 1e-9);
    }
}
