//! Lining two sequences up in time, so frame `i` of ours is compared with the reference frame it is meant to be: the
//! single offset that fits best, and the dynamic time warp (DTW) that maps every frame of ours to one of the reference,
//! with the drift it shows. Works on any per-frame cost, so video frames and signal samples use the same code.

use schemars::JsonSchema;
use serde::Serialize;

use super::raster::LabImage;
use super::score::round;

/// The side of the grid a frame is summarised on for timing.
const GRID: usize = 24;

/// A frame's timing descriptor: L* and coverage averaged on a 24 x 24 grid.
#[must_use]
#[allow(clippy::cast_precision_loss)] // cell counts
pub fn descriptor(image: &LabImage) -> Vec<f32> {
    let mut sums = vec![0.0f32; GRID * GRID * 2];
    let mut counts = vec![0u32; GRID * GRID];
    for y in 0..image.h {
        let cy = y * GRID / image.h;
        for x in 0..image.w {
            let c = cy * GRID + x * GRID / image.w;
            let i = y * image.w + x;
            sums[2 * c] += image.l.v[i];
            sums[2 * c + 1] += image.alpha.v[i] * 100.0;
            counts[c] += 1;
        }
    }
    for (c, n) in counts.iter().enumerate() {
        let n = (*n).max(1) as f32;
        sums[2 * c] /= n;
        sums[2 * c + 1] /= n;
    }
    sums
}

/// The mean absolute difference of two descriptors.
#[must_use]
#[allow(clippy::cast_precision_loss)] // descriptor lengths
pub fn descriptor_cost(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len().max(1) as f32
}

/// How the two sequences line up in time.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct Timing {
    /// The reference frame that ours' first frame matches best with one fixed offset (negative: ours starts later).
    pub offset_frames: i64,
    /// The mean cost at that offset.
    pub offset_cost: f64,
    /// For each frame of ours, the reference frame the time warp matches it with.
    pub map: Vec<usize>,
    /// The mean of `map[i] - (i + offset)`: how far, on average, the warp moves a frame from the fixed offset.
    pub drift_mean_frames: f64,
    /// The largest such move, in frames (signed).
    pub drift_max_frames: i64,
    /// Reference frames per frame of ours along the warp (the slope of a line through the map): 1 when the two run at
    /// one speed, below 1 when ours runs slow (holds frames), above 1 when it runs fast.
    pub rate: f64,
    /// The mean cost along the warp.
    pub warp_cost: f64,
}

/// The fixed offset in `-max_lag..=max_lag` with the least mean cost, over offsets that overlap at least half of the
/// shorter sequence.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap, clippy::cast_sign_loss)] // sequence lengths
pub fn best_offset(cost: &dyn Fn(usize, usize) -> f32, n_ours: usize, n_ref: usize, max_lag: usize) -> (i64, f64) {
    let need = (n_ours.min(n_ref) / 2).max(1);
    let mut best = (0i64, f64::INFINITY);
    let lag = max_lag as i64;
    for off in -lag..=lag {
        let (mut sum, mut n) = (0.0f64, 0usize);
        for i in 0..n_ours {
            let j = i as i64 + off;
            if j < 0 || j >= n_ref as i64 {
                continue;
            }
            sum += f64::from(cost(i, usize::try_from(j).unwrap_or(0)));
            n += 1;
        }
        if n >= need {
            let mean = sum / n as f64;
            if mean < best.1 - 1e-12 || (mean - best.1).abs() <= 1e-12 && off.abs() < best.0.abs() {
                best = (off, mean);
            }
        }
    }
    best
}

/// The dynamic time warp of ours against the reference: for each frame of ours, the reference frame on the least-cost
/// monotonic path (steps of one frame on either side or both), and the path's mean cost. The path may start and end
/// anywhere in the reference (a subsequence warp), so a clip is placed inside a longer reference.
#[must_use]
#[allow(clippy::cast_precision_loss)] // sequence lengths
pub fn warp(cost: &dyn Fn(usize, usize) -> f32, n_ours: usize, n_ref: usize) -> (Vec<usize>, f64) {
    if n_ours == 0 || n_ref == 0 {
        return (Vec::new(), 0.0);
    }
    let c: Vec<f32> = (0..n_ours * n_ref).map(|k| cost(k / n_ref, k % n_ref)).collect();
    let mut acc = vec![f64::INFINITY; n_ours * n_ref];
    for i in 0..n_ours {
        for j in 0..n_ref {
            let here = f64::from(c[i * n_ref + j]);
            let prev = if i == 0 {
                0.0
            } else {
                let mut p = f64::INFINITY;
                if i > 0 {
                    p = p.min(acc[(i - 1) * n_ref + j]);
                }
                if j > 0 {
                    p = p.min(acc[i * n_ref + j - 1]);
                }
                if i > 0 && j > 0 {
                    p = p.min(acc[(i - 1) * n_ref + j - 1]);
                }
                p
            };
            acc[i * n_ref + j] = here + prev;
        }
    }
    // Walk back, keeping for each frame of ours its cheapest reference frame on the path.
    let mut map = vec![usize::MAX; n_ours];
    let last = &acc[(n_ours - 1) * n_ref..];
    let end = (0..n_ref).fold(0, |best, j| if last[j] < last[best] { j } else { best });
    let (mut i, mut j) = (n_ours - 1, end);
    let (mut sum, mut steps) = (0.0f64, 0usize);
    loop {
        sum += f64::from(c[i * n_ref + j]);
        steps += 1;
        if map[i] == usize::MAX || c[i * n_ref + j] < c[i * n_ref + map[i]] {
            map[i] = j;
        }
        if i == 0 {
            break;
        }
        let diag = if i > 0 && j > 0 { acc[(i - 1) * n_ref + j - 1] } else { f64::INFINITY };
        let up = if i > 0 { acc[(i - 1) * n_ref + j] } else { f64::INFINITY };
        let left = if j > 0 { acc[i * n_ref + j - 1] } else { f64::INFINITY };
        if diag <= up && diag <= left {
            i -= 1;
            j -= 1;
        } else if up <= left {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    (map, sum / steps.max(1) as f64)
}

/// The timing of ours against the reference from a per-frame cost.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap, clippy::cast_possible_truncation)] // sequence lengths
pub fn timing(cost: &dyn Fn(usize, usize) -> f32, n_ours: usize, n_ref: usize, max_lag: usize) -> Timing {
    let (offset, offset_cost) = best_offset(cost, n_ours, n_ref, max_lag);
    let (map, warp_cost) = warp(cost, n_ours, n_ref);
    let drift: Vec<f64> = map.iter().enumerate().map(|(i, j)| *j as f64 - (i as f64 + offset as f64)).collect();
    let n = drift.len().max(1) as f64;
    let drift_max = drift.iter().copied().fold(0.0f64, |m, d| if d.abs() > m.abs() { d } else { m });
    // The least-squares slope of the map against frame index.
    let mean_i = (map.len() as f64 - 1.0) / 2.0;
    let mean_j = map.iter().map(|j| *j as f64).sum::<f64>() / n;
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for (i, j) in map.iter().enumerate() {
        num += (i as f64 - mean_i) * (*j as f64 - mean_j);
        den += (i as f64 - mean_i).powi(2);
    }
    Timing {
        offset_frames: offset,
        offset_cost: round(offset_cost),
        map,
        drift_mean_frames: round(drift.iter().sum::<f64>() / n),
        drift_max_frames: drift_max.round() as i64,
        rate: round(if den > 0.0 { num / den } else { 1.0 }),
        warp_cost: round(warp_cost),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shifted_sequence_is_found_at_its_offset() {
        let reference: Vec<f32> = (0..40).map(|i| ((i as f32) * 0.7).sin()).collect();
        let ours: Vec<f32> = reference[5..35].to_vec();
        let cost = |i: usize, j: usize| (ours[i] - reference[j]).abs();
        let t = timing(&cost, ours.len(), reference.len(), 10);
        assert_eq!(t.offset_frames, 5);
        assert!(t.offset_cost < 1e-6);
        assert_eq!(t.map[0], 5);
        assert_eq!(t.drift_max_frames, 0);
        assert!((t.rate - 1.0).abs() < 1e-9, "{}", t.rate);
    }

    #[test]
    fn a_slow_sequence_shows_its_rate() {
        // Ours holds every reference frame twice: it plays at half speed.
        let reference: Vec<f32> = (0..20).map(|i| i as f32).collect();
        let ours: Vec<f32> = (0..40).map(|i| (i / 2) as f32).collect();
        let cost = |i: usize, j: usize| (ours[i] - reference[j]).abs();
        let t = timing(&cost, ours.len(), reference.len(), 5);
        assert!((t.rate - 0.5).abs() < 0.02, "{}", t.rate);
        assert!(t.warp_cost < 1e-6);
    }
}
