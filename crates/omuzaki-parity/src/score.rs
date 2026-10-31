//! The score of one render against one reference on one pixel grid, in the result shape every parity measure shares
//! (images, signals, voice): the metrics with their targets, `done`, and the worst places, each with a hint.
//!
//! **The stop rule.** Parity is reached when the exact difference map is 0: every scored pixel's sRGB bytes match
//! (within `tolerance_levels`, 0 by default). `done` is that and nothing else. The other numbers measure how far there
//! is to go and where, and their targets are their values at parity.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::align::Alignment;
use super::metrics::{delta_e_map, edge_offset, edges, gms_map, iou, level_map, share, ssim_map, stats};
use super::raster::{LabImage, Plane};
use super::regions::Region;

/// One measure in the shared result shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metric {
    /// What is measured, such as `pixels_off`.
    pub name: String,
    /// The value.
    pub value: f64,
    /// Its unit.
    pub unit: String,
    /// The value at parity.
    pub target: f64,
    /// `lower` or `higher`: which way is better.
    pub better: String,
    /// Whether the value is at the target.
    pub pass: bool,
    /// `all`, or the region, frame, channel or segment it is about.
    pub scope: String,
}

impl Metric {
    /// A measure whose best value is `target`, lower or higher being better.
    #[must_use]
    pub fn new(name: &str, value: f64, unit: &str, target: f64, lower_is_better: bool, scope: &str) -> Self {
        let value = round(value);
        let pass = if lower_is_better { value <= target + 1e-9 } else { value >= target - 1e-9 };
        Self {
            name: name.to_owned(),
            value,
            unit: unit.to_owned(),
            target,
            better: if lower_is_better { "lower" } else { "higher" }.to_owned(),
            pass,
            scope: scope.to_owned(),
        }
    }
}

/// One of the worst places, in the shared result shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Worst {
    /// Where: a region, a frame, a channel, a tile.
    pub scope: String,
    /// The measure that is off there.
    pub metric: String,
    /// Its value.
    pub value: f64,
    /// Its value at parity.
    pub target: f64,
    /// The share of all the error that is there (0 to 1).
    pub share_of_error: f64,
    /// What to change, in a line.
    pub hint: String,
}

/// Rounds to 6 decimals, so outputs read cleanly and stay byte-stable.
#[must_use]
pub fn round(v: f64) -> f64 {
    if v.is_finite() { (v * 1e6).round() / 1e6 } else { v }
}

/// How a pair is scored.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreOptions {
    /// The largest byte difference that still counts as a match (0: exact).
    pub tolerance_levels: u8,
    /// How many worst tiles to report.
    pub tiles: usize,
    /// The tile side in pixels (0: about a 24th of the longer side).
    pub tile_px: usize,
}

impl Default for ScoreOptions {
    fn default() -> Self {
        Self { tolerance_levels: 0, tiles: 8, tile_px: 0 }
    }
}

/// A region's colour, as the two images have it on average.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ColourGap {
    /// Our mean colour, `#rrggbb`.
    pub ours: String,
    /// The reference's mean colour.
    pub reference: String,
    /// Our mean L* minus the reference's (positive: ours is lighter).
    pub delta_lightness: f64,
    /// Our mean chroma minus the reference's (positive: ours is more saturated).
    pub delta_chroma: f64,
    /// The hue turn from the reference's mean to ours, degrees.
    pub delta_hue_deg: f64,
    /// The share of the region's colour error a uniform colour shift would remove (0 to 1): high means a colour or
    /// material change, low means shape, shading or detail.
    pub shift_explains: f64,
}

/// One region's score.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct RegionScore {
    /// The region.
    pub name: String,
    /// `found` or `given`.
    pub source: String,
    /// How many pixels it holds.
    pub pixels: usize,
    /// How many of them are off (the stop rule's map).
    pub pixels_off: usize,
    /// The share of its pixels that are off.
    pub off_share: f64,
    /// The share within CIEDE2000 2.3 (just noticeable).
    pub within_jnd: f64,
    /// The mean CIEDE2000.
    pub delta_e_mean: f64,
    /// The 95th percentile CIEDE2000.
    pub delta_e_p95: f64,
    /// The mean SSIM on L*.
    pub ssim: f64,
    /// The mean distance between the two images' edges, pixels (`None` without edges).
    pub edge_offset_px: Option<f64>,
    /// The silhouettes' overlap inside the region.
    pub silhouette_iou: f64,
    /// The share of all the frame's colour error that lies in it.
    pub share_of_error: f64,
    /// Its colour gap.
    pub colour: ColourGap,
    /// What to change, in a line.
    pub hint: String,
}

/// A tile of the frame.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct Tile {
    /// `[x, y, w, h]` in pixels.
    pub rect: [usize; 4],
    /// The mean CIEDE2000 in it.
    pub delta_e_mean: f64,
    /// The share of its pixels that are off.
    pub off_share: f64,
    /// The region most of it lies in.
    pub region: String,
}

/// The score of a pair.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct PairScore {
    /// `image`.
    pub track: String,
    /// True only when the exact difference map is 0 over every scored pixel.
    pub done: bool,
    /// `[width, height]`.
    pub size: [usize; 2],
    /// The scored pixels.
    pub pixels: usize,
    /// The pixels left out by `ignore` regions.
    pub ignored: usize,
    /// The scored pixels that are off.
    pub pixels_off: usize,
    /// The largest per-channel byte difference over the scored pixels (0 at parity).
    pub max_level: u8,
    /// The whole frame's measures.
    pub metrics: Vec<Metric>,
    /// Each region's score.
    pub regions: Vec<RegionScore>,
    /// The worst regions, worst first.
    pub worst: Vec<Worst>,
    /// The worst tiles, worst first.
    pub worst_tiles: Vec<Tile>,
    /// How far the camera is off, when alignment was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alignment: Option<AlignedScore>,
}

/// The score again after lining ours up: what share of the error is framing.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct AlignedScore {
    /// The transform and the camera change it asks for.
    #[serde(flatten)]
    pub alignment: Alignment,
    /// The share within CIEDE2000 2.3 once aligned.
    pub within_jnd_aligned: f64,
    /// The mean CIEDE2000 once aligned.
    pub delta_e_mean_aligned: f64,
    /// The share of the colour error that the camera change would remove.
    pub framing_explains: f64,
}

/// Scores ours against the reference (one size). `keep` marks the scored pixels (none: all); `regions` are scored
/// each; `silhouettes` are ours and the reference's.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)] // pixel counts; one pass over the measures
pub fn score_pair(
    ours: &LabImage,
    reference: &LabImage,
    keep: &[bool],
    silhouettes: (&[bool], &[bool]),
    regions: &[Region],
    options: &ScoreOptions,
) -> PairScore {
    let (w, h) = (ours.w, ours.h);
    let tol = f32::from(options.tolerance_levels);
    let levels = level_map(ours, reference);
    let de = delta_e_map(ours, reference);
    let ssim = ssim_map(&ours.l, &reference.l);
    let gms = gms_map(&ours.l, &reference.l);
    let (eo, er) = (edges(&ours.l), edges(&reference.l));
    let off: Vec<bool> = levels.v.iter().zip(keep).map(|(d, k)| *k && *d > tol).collect();
    let pixels = keep.iter().filter(|k| **k).count();
    let pixels_off = off.iter().filter(|o| **o).count();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a byte difference
    let max_level = levels.v.iter().zip(keep).filter(|(_, k)| **k).map(|(d, _)| *d as u8).max().unwrap_or(0);
    let total_error: f64 = de.v.iter().zip(keep).filter(|(_, k)| **k).map(|(d, _)| f64::from(*d)).sum();
    let all = Some(keep);
    let de_stats = stats(&de, all);
    let mse = {
        let mut sum = 0.0f64;
        for (i, (p, q)) in ours.rgb.iter().zip(&reference.rgb).enumerate() {
            if keep[i] {
                sum += (0..3).map(|k| (f64::from(p[k]) - f64::from(q[k])).powi(2)).sum::<f64>() / 3.0;
            }
        }
        sum / pixels.max(1) as f64
    };
    let psnr = if mse <= 1e-12 { 99.0 } else { 10.0 * (255.0f64.powi(2) / mse).log10() };
    let gms_stats = stats(&gms, all);
    let gmsd = {
        let mut var = 0.0f64;
        for (i, g) in gms.v.iter().enumerate() {
            if keep[i] {
                var += (f64::from(*g) - gms_stats.mean).powi(2);
            }
        }
        (var / pixels.max(1) as f64).sqrt()
    };
    let (so, sr) = silhouettes;
    let sil_iou = iou(so, sr, all);
    let edge_px = edge_offset(&eo, &er, w, h, all, 50.0);
    let mut metrics = vec![
        Metric::new("pixels_off", pixels_off as f64, "px", 0.0, true, "all"),
        Metric::new("off_share", pixels_off as f64 / pixels.max(1) as f64, "share", 0.0, true, "all"),
        Metric::new("within_jnd", share(&de, all, |d| d <= 2.3), "share", 1.0, false, "all"),
        Metric::new("delta_e_mean", de_stats.mean, "CIEDE2000", 0.0, true, "all"),
        Metric::new("delta_e_p95", de_stats.p95, "CIEDE2000", 0.0, true, "all"),
        Metric::new("psnr_db", psnr, "dB", 99.0, false, "all"),
        Metric::new("ssim", stats(&ssim, all).mean, "SSIM", 1.0, false, "all"),
        Metric::new("gmsd", gmsd, "GMSD", 0.0, true, "all"),
        Metric::new("silhouette_iou", sil_iou, "IoU", 1.0, false, "all"),
    ];
    if let Some(e) = edge_px {
        metrics.push(Metric::new("edge_offset_px", e, "px", 0.0, true, "all"));
    }
    let mut scored = Vec::new();
    for r in regions {
        let mask: Vec<bool> = r.mask.iter().zip(keep).map(|(m, k)| *m && *k).collect();
        let n = mask.iter().filter(|m| **m).count();
        if n == 0 {
            continue;
        }
        let m = Some(mask.as_slice());
        let d = stats(&de, m);
        let region_off = off.iter().zip(&mask).filter(|(o, m)| **o && **m).count();
        let colour = colour_gap(ours, reference, &mask, &de);
        let ssim_mean = stats(&ssim, m).mean;
        let edge_px = edge_offset(&eo, &er, w, h, m, 50.0);
        let sil = iou(so, sr, m);
        let share_of_error = if total_error > 0.0 { d.sum / total_error } else { 0.0 };
        let hint = hint(&r.name, &colour, ssim_mean, edge_px, sil, region_off);
        scored.push(RegionScore {
            name: r.name.clone(),
            source: r.source.to_owned(),
            pixels: n,
            pixels_off: region_off,
            off_share: round(region_off as f64 / n as f64),
            within_jnd: round(share(&de, m, |v| v <= 2.3)),
            delta_e_mean: round(d.mean),
            delta_e_p95: round(d.p95),
            ssim: round(ssim_mean),
            edge_offset_px: edge_px.map(round),
            silhouette_iou: round(sil),
            share_of_error: round(share_of_error),
            colour,
            hint,
        });
    }
    // The worst regions by their share of the error; the silhouette is the sum of the parts, so it is left out.
    let mut worst: Vec<Worst> = scored
        .iter()
        .filter(|r| r.name != "silhouette" && r.pixels_off > 0)
        .map(|r| Worst {
            scope: r.name.clone(),
            metric: "delta_e_mean".to_owned(),
            value: r.delta_e_mean,
            target: 0.0,
            share_of_error: r.share_of_error,
            hint: r.hint.clone(),
        })
        .collect();
    worst.sort_by(|a, b| b.share_of_error.total_cmp(&a.share_of_error));
    let worst_tiles = tiles(&de, &off, keep, regions, options);
    PairScore {
        track: "image".to_owned(),
        done: pixels_off == 0,
        size: [w, h],
        pixels,
        ignored: w * h - pixels,
        pixels_off,
        max_level,
        metrics,
        regions: scored,
        worst,
        worst_tiles,
        alignment: None,
    }
}

/// The worst tiles by mean CIEDE2000.
#[allow(clippy::cast_precision_loss)] // pixel counts
fn tiles(de: &Plane, off: &[bool], keep: &[bool], regions: &[Region], options: &ScoreOptions) -> Vec<Tile> {
    let (w, h) = (de.w, de.h);
    let side = if options.tile_px > 0 { options.tile_px } else { (w.max(h) / 24).max(8) };
    let mut out = Vec::new();
    for ty in (0..h).step_by(side) {
        for tx in (0..w).step_by(side) {
            let (tw, th) = (side.min(w - tx), side.min(h - ty));
            let (mut sum, mut n, mut n_off) = (0.0f64, 0usize, 0usize);
            for y in ty..ty + th {
                for x in tx..tx + tw {
                    let i = y * w + x;
                    if keep[i] {
                        sum += f64::from(de.v[i]);
                        n += 1;
                        n_off += usize::from(off[i]);
                    }
                }
            }
            if n == 0 || n_off == 0 {
                continue;
            }
            // The most specific region the tile lies in: the parts before the silhouette and the outline.
            let region = regions
                .iter()
                .filter(|r| !matches!(r.name.as_str(), "silhouette" | "outline"))
                .map(|r| {
                    let c = (ty..ty + th)
                        .flat_map(|y| (tx..tx + tw).map(move |x| y * w + x))
                        .filter(|i| r.mask[*i])
                        .count();
                    (c, r.name.as_str())
                })
                .max_by_key(|(c, _)| *c)
                .filter(|(c, _)| *c > 0)
                .map_or("frame", |(_, n)| n);
            out.push(Tile {
                rect: [tx, ty, tw, th],
                delta_e_mean: round(sum / n as f64),
                off_share: round(n_off as f64 / n as f64),
                region: region.to_owned(),
            });
        }
    }
    out.sort_by(|a, b| b.delta_e_mean.total_cmp(&a.delta_e_mean));
    out.truncate(options.tiles);
    out
}

/// The mean colours of a region on both sides, and how much of its error a uniform shift would remove.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // pixel counts; Lab fits f32
fn colour_gap(ours: &LabImage, reference: &LabImage, mask: &[bool], de: &Plane) -> ColourGap {
    let mut sum_o = [0.0f64; 3];
    let mut sum_r = [0.0f64; 3];
    let mut n = 0.0f64;
    for (i, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
        let (o, r) = (ours.lab_at(i), reference.lab_at(i));
        for k in 0..3 {
            sum_o[k] += f64::from(o[k]);
            sum_r[k] += f64::from(r[k]);
        }
        n += 1.0;
    }
    let mo = sum_o.map(|v| v / n.max(1.0));
    let mr = sum_r.map(|v| v / n.max(1.0));
    let shift = [mr[0] - mo[0], mr[1] - mo[1], mr[2] - mo[2]];
    let (mut before, mut after) = (0.0f64, 0.0f64);
    for (i, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
        let o = ours.lab_at(i).map(f64::from);
        let r = reference.lab_at(i).map(f64::from);
        before += f64::from(de.v[i]);
        after += super::metrics::ciede2000([o[0] + shift[0], o[1] + shift[1], o[2] + shift[2]], r);
    }
    let chroma = |c: [f64; 3]| c[1].hypot(c[2]);
    let hue = |c: [f64; 3]| c[2].atan2(c[1]).to_degrees();
    let mut dh = hue(mo) - hue(mr);
    if dh > 180.0 {
        dh -= 360.0;
    } else if dh < -180.0 {
        dh += 360.0;
    }
    if chroma(mo) < 2.0 || chroma(mr) < 2.0 {
        dh = 0.0; // a grey has no hue to turn
    }
    ColourGap {
        ours: crate::colour::hex(srgb(mo)),
        reference: crate::colour::hex(srgb(mr)),
        delta_lightness: round(mo[0] - mr[0]),
        delta_chroma: round(chroma(mo) - chroma(mr)),
        delta_hue_deg: round(dh),
        shift_explains: round(if before > 0.0 { ((before - after) / before).max(0.0) } else { 0.0 }),
    }
}

/// CIELAB (D65) to sRGB bytes, clamped to the gamut.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to a byte
pub fn srgb(lab: [f64; 3]) -> [u8; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let inv = |t: f64| if t.powi(3) > 216.0 / 24389.0 { t.powi(3) } else { (116.0 * t - 16.0) / (24389.0 / 27.0) };
    let (x, y, z) = (0.950_47 * inv(fx), inv(fy), 1.088_83 * inv(fz));
    let r = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
    let g = -0.969_266 * x + 1.876_010_8 * y + 0.041_556 * z;
    let b = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;
    let gamma = |c: f64| {
        let c = c.clamp(0.0, 1.0);
        let v = if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [gamma(r), gamma(g), gamma(b)]
}

/// The one-line hint for a region.
fn hint(name: &str, c: &ColourGap, ssim: f64, edge_px: Option<f64>, sil_iou: f64, off: usize) -> String {
    if off == 0 {
        return format!("{name}: every pixel matches");
    }
    let mut parts = Vec::new();
    if sil_iou < 0.995 && name != "background" {
        parts.push(format!(
            "shape: the silhouettes overlap {:.1}% here; fix the outline, size and placement first",
            sil_iou * 100.0
        ));
    }
    if let Some(e) = edge_px.filter(|e| *e > 0.75) {
        parts.push(format!("lines sit {e:.1} px apart on average (placement, proportion or line work)"));
    }
    if c.delta_lightness.abs() >= 1.0 {
        parts.push(format!(
            "ours is {:.1} L* {}",
            c.delta_lightness.abs(),
            if c.delta_lightness > 0.0 { "lighter" } else { "darker" }
        ));
    }
    if c.delta_chroma.abs() >= 1.0 {
        parts.push(format!(
            "{:.1} chroma {}",
            c.delta_chroma.abs(),
            if c.delta_chroma > 0.0 { "more saturated" } else { "less saturated" }
        ));
    }
    if c.delta_hue_deg.abs() >= 2.0 {
        parts.push(format!("hue turned {:.0} degrees", c.delta_hue_deg));
    }
    if c.shift_explains >= 0.3 {
        parts.push(format!(
            "one colour change across it fixes {:.0}%: aim its mean at {} (ours {})",
            c.shift_explains * 100.0,
            c.reference,
            c.ours
        ));
    }
    if c.shift_explains < 0.5 && ssim < 0.9 {
        parts.push(format!("structure differs (SSIM {ssim:.2}): shading, texture, strands or detail"));
    }
    if parts.is_empty() {
        parts.push("small differences everywhere: shading, antialiasing or noise".to_owned());
    }
    format!("{name}: {}", parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_round_trips_through_srgb() {
        for c in [[0u8, 0, 0], [255, 255, 255], [200, 30, 90], [12, 140, 220]] {
            let back = srgb(crate::colour::lab(c));
            assert!((0..3).all(|k| back[k].abs_diff(c[k]) <= 1), "{c:?} -> {back:?}");
        }
    }

    #[test]
    fn a_metric_passes_only_at_its_target() {
        assert!(Metric::new("pixels_off", 0.0, "px", 0.0, true, "all").pass);
        assert!(!Metric::new("pixels_off", 1.0, "px", 0.0, true, "all").pass);
        assert!(!Metric::new("ssim", 0.9999, "SSIM", 1.0, false, "all").pass);
    }
}
