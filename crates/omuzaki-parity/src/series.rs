//! Parity for the tracks that are signals, not pictures: tracking blendshapes and head pose, rig and motion curves,
//! joint angles, latency traces, any per-frame numbers. Ours and the reference are lined up in time (a fixed offset,
//! optionally a warp), then compared sample by sample, in the shared result shape. As with pixels, `done` is true only
//! when every sample of every channel matches (within `tolerance`, 0 by default).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::score::{Metric, Worst, round};
use super::timing::{Timing, timing};

/// A set of channels sampled at one rate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Series {
    /// Samples a second.
    pub rate_hz: f64,
    /// Each channel's samples, by name (such as `jawOpen` or `head.yaw`).
    pub channels: BTreeMap<String, Vec<f64>>,
}

/// How the series are lined up in time.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimeAlign {
    /// Sample `i` against sample `i`.
    None,
    /// The single offset that fits best.
    #[default]
    Offset,
    /// A time warp: each sample of ours against the reference sample the warp matches it with.
    Warp,
}

/// One channel's score.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct ChannelScore {
    /// The channel.
    pub name: String,
    /// The samples compared.
    pub samples: usize,
    /// The samples further apart than the tolerance.
    pub samples_off: usize,
    /// Root mean square error.
    pub rmse: f64,
    /// The largest difference.
    pub max_abs: f64,
    /// Pearson correlation (1: the same shape).
    pub correlation: f64,
    /// Our standard deviation over the reference's (1: the same amplitude).
    pub amplitude_ratio: f64,
    /// The lag at which this channel alone fits best, milliseconds (positive: ours is late).
    pub lag_ms: f64,
    /// The share of all the squared error that lies in this channel.
    pub share_of_error: f64,
}

/// The score of two series.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct SeriesScore {
    /// `signal`.
    pub track: String,
    /// True only when every channel is present on both sides and every sample matches within the tolerance.
    pub done: bool,
    /// The tolerance used.
    pub tolerance: f64,
    /// The time alignment.
    pub timing: Timing,
    /// The offset in milliseconds (positive: ours starts later in the reference).
    pub offset_ms: f64,
    /// The whole set's measures.
    pub metrics: Vec<Metric>,
    /// Each channel's score.
    pub channels: Vec<ChannelScore>,
    /// Channels only one side has.
    pub missing: Vec<String>,
    /// The worst channels and windows, worst first.
    pub worst: Vec<Worst>,
}

/// Scores ours against the reference.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::too_many_lines
)]
pub fn score_series(
    ours: &Series,
    reference: &Series,
    tolerance: f64,
    align: TimeAlign,
    max_lag_s: f64,
) -> SeriesScore {
    let names: Vec<&String> = ours.channels.keys().filter(|k| reference.channels.contains_key(*k)).collect();
    let mut missing: Vec<String> = ours
        .channels
        .keys()
        .chain(reference.channels.keys())
        .filter(|k| !(ours.channels.contains_key(*k) && reference.channels.contains_key(*k)))
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    let n_ours = names.iter().map(|n| ours.channels[*n].len()).min().unwrap_or(0);
    let n_ref = names.iter().map(|n| reference.channels[*n].len()).min().unwrap_or(0);
    // Each channel weighs by the reference's spread, so a channel in degrees does not drown one in 0..1.
    let spread: Vec<f64> = names
        .iter()
        .map(|n| {
            let v = &reference.channels[*n];
            let m = v.iter().sum::<f64>() / v.len().max(1) as f64;
            (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt().max(1e-6)
        })
        .collect();
    let cost = |i: usize, j: usize| -> f32 {
        let mut s = 0.0f64;
        for (k, n) in names.iter().enumerate() {
            s += ((ours.channels[*n][i] - reference.channels[*n][j]) / spread[k]).abs();
        }
        (s / names.len().max(1) as f64) as f32
    };
    let max_lag =
        if matches!(align, TimeAlign::None) { 0 } else { (max_lag_s * reference.rate_hz).round().max(0.0) as usize };
    let t = timing(&cost, n_ours, n_ref, max_lag);
    let partner = |i: usize| -> Option<usize> {
        match align {
            TimeAlign::None => (i < n_ref).then_some(i),
            TimeAlign::Offset => {
                let j = i as i64 + t.offset_frames;
                (j >= 0 && (j as usize) < n_ref).then_some(j as usize)
            }
            TimeAlign::Warp => t.map.get(i).copied(),
        }
    };
    let mut channels = Vec::new();
    let mut total_sq = 0.0f64;
    let mut windows: Vec<Worst> = Vec::new();
    let window = (reference.rate_hz * 0.5).round().max(1.0) as usize;
    for n in &names {
        let (a, b) = (&ours.channels[*n], &reference.channels[*n]);
        let pairs: Vec<(usize, f64, f64)> = (0..n_ours).filter_map(|i| partner(i).map(|j| (i, a[i], b[j]))).collect();
        let m = pairs.len().max(1) as f64;
        let sq: f64 = pairs.iter().map(|(_, x, y)| (x - y).powi(2)).sum();
        total_sq += sq;
        let off = pairs.iter().filter(|(_, x, y)| (x - y).abs() > tolerance).count();
        let max_abs = pairs.iter().map(|(_, x, y)| (x - y).abs()).fold(0.0, f64::max);
        let (ma, mb) = (pairs.iter().map(|p| p.1).sum::<f64>() / m, pairs.iter().map(|p| p.2).sum::<f64>() / m);
        let (va, vb) = (
            pairs.iter().map(|p| (p.1 - ma).powi(2)).sum::<f64>() / m,
            pairs.iter().map(|p| (p.2 - mb).powi(2)).sum::<f64>() / m,
        );
        let cov = pairs.iter().map(|p| (p.1 - ma) * (p.2 - mb)).sum::<f64>() / m;
        let correlation = if va > 1e-12 && vb > 1e-12 {
            cov / (va * vb).sqrt()
        } else if va <= 1e-12 && vb <= 1e-12 {
            1.0
        } else {
            0.0
        };
        // This channel's own best lag.
        let single = |i: usize, j: usize| (a[i] - b[j]).abs() as f32;
        let (lag, _) = super::timing::best_offset(&single, n_ours.min(a.len()), n_ref.min(b.len()), max_lag);
        channels.push(ChannelScore {
            name: (*n).clone(),
            samples: pairs.len(),
            samples_off: off,
            rmse: round((sq / m).sqrt()),
            max_abs: round(max_abs),
            correlation: round(correlation),
            amplitude_ratio: round(if vb > 1e-12 { (va / vb).sqrt() } else { 1.0 }),
            lag_ms: round((lag - t.offset_frames) as f64 * 1000.0 / reference.rate_hz),
            share_of_error: sq,
        });
        // Its worst half-second window.
        if pairs.len() >= window {
            let mut best = (0usize, 0.0f64);
            for s in 0..=pairs.len() - window {
                let e = pairs[s..s + window].iter().map(|(_, x, y)| (x - y).abs()).sum::<f64>() / window as f64;
                if e > best.1 {
                    best = (s, e);
                }
            }
            if best.1 > tolerance {
                let start = pairs[best.0].0 as f64 / ours.rate_hz;
                windows.push(Worst {
                    scope: format!("{n} {start:.2}-{:.2} s", start + window as f64 / ours.rate_hz),
                    metric: "mean_abs".to_owned(),
                    value: round(best.1),
                    target: tolerance,
                    share_of_error: 0.0,
                    hint: String::new(),
                });
            }
        }
    }
    for c in &mut channels {
        c.share_of_error = round(if total_sq > 0.0 { c.share_of_error / total_sq } else { 0.0 });
    }
    let mut worst: Vec<Worst> = channels
        .iter()
        .filter(|c| c.samples_off > 0)
        .map(|c| Worst {
            scope: c.name.clone(),
            metric: "rmse".to_owned(),
            value: c.rmse,
            target: 0.0,
            share_of_error: c.share_of_error,
            hint: channel_hint(c),
        })
        .collect();
    worst.sort_by(|a, b| b.share_of_error.total_cmp(&a.share_of_error));
    windows.sort_by(|a, b| b.value.total_cmp(&a.value));
    worst.extend(windows.into_iter().take(5));
    let samples: usize = channels.iter().map(|c| c.samples).sum();
    let samples_off: usize = channels.iter().map(|c| c.samples_off).sum();
    let length_gap = n_ours.abs_diff(n_ref);
    let rmse = (total_sq / samples.max(1) as f64).sqrt();
    let mean_corr = channels.iter().map(|c| c.correlation).sum::<f64>() / channels.len().max(1) as f64;
    let metrics = vec![
        Metric::new("samples_off", samples_off as f64, "samples", 0.0, true, "all"),
        Metric::new("rmse", rmse, "units", 0.0, true, "all"),
        Metric::new("correlation_mean", mean_corr, "r", 1.0, false, "all"),
        Metric::new("missing_channels", missing.len() as f64, "channels", 0.0, true, "all"),
        Metric::new("length_gap", length_gap as f64, "samples", 0.0, true, "all"),
        Metric::new("offset_ms", (t.offset_frames as f64 * 1000.0 / reference.rate_hz).abs(), "ms", 0.0, true, "all"),
    ];
    let done = metrics.iter().all(|m| m.pass) && samples > 0;
    SeriesScore {
        track: "signal".to_owned(),
        done,
        tolerance,
        offset_ms: round(t.offset_frames as f64 * 1000.0 / reference.rate_hz),
        timing: t,
        metrics,
        channels,
        missing,
        worst,
    }
}

fn channel_hint(c: &ChannelScore) -> String {
    let mut parts = Vec::new();
    if c.lag_ms.abs() >= 1.0 {
        parts.push(format!("{:.0} ms {}", c.lag_ms.abs(), if c.lag_ms > 0.0 { "late" } else { "early" }));
    }
    if (c.amplitude_ratio - 1.0).abs() >= 0.05 {
        parts.push(format!("amplitude {:.0}% of the reference's", c.amplitude_ratio * 100.0));
    }
    if c.correlation < 0.9 {
        parts.push(format!("a different shape (r {:.2})", c.correlation));
    }
    if parts.is_empty() {
        parts.push(format!("off by up to {:.4}", c.max_abs));
    }
    format!("{}: {}", c.name, parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(f: impl Fn(usize) -> f64, n: usize) -> Series {
        Series { rate_hz: 30.0, channels: [("jawOpen".to_owned(), (0..n).map(&f).collect())].into_iter().collect() }
    }

    #[test]
    fn identical_series_are_done() {
        let s = series(|i| (i as f64 * 0.3).sin(), 60);
        let score = score_series(&s, &s, 0.0, TimeAlign::Offset, 1.0);
        assert!(score.done, "{score:?}");
        assert_eq!(score.offset_ms, 0.0);
    }

    #[test]
    fn a_late_and_damped_channel_reads_as_such() {
        // A chirp, so no other offset fits as well.
        let reference = series(|i| (0.004 * (i as f64).powi(2)).sin(), 90);
        let ours = series(|i| 0.5 * (0.004 * (i as f64 + 3.0).powi(2)).sin(), 60);
        let score = score_series(&ours, &reference, 0.0, TimeAlign::Offset, 1.0);
        assert!(!score.done);
        assert_eq!(score.timing.offset_frames, 3, "{:?}", score.timing);
        let c = &score.channels[0];
        assert!((c.amplitude_ratio - 0.5).abs() < 0.01, "{c:?}");
        assert!(score.worst[0].hint.contains("amplitude 50%"), "{}", score.worst[0].hint);
    }

    #[test]
    fn huge_values_score_without_overflowing() {
        let reference = series(|i| [0.0, 0.25, 0.5][i], 3);
        for big in [[0.0, 1e300, 0.5], [1e300, 0.2, 0.5], [-1e300, 1e300, f64::MAX]] {
            let ours = series(|i| big[i], 3);
            for align in [TimeAlign::None, TimeAlign::Offset, TimeAlign::Warp] {
                let score = score_series(&ours, &reference, 0.0, align, 1.0);
                assert!(!score.done, "{score:?}");
            }
        }
    }
}
