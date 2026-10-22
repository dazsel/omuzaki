//! What `audio.fx.render` and `audio.fx.compare` report about a rendered voice (design section 6.2): the median F0,
//! the first three formants, the integrated loudness, the spectral centroid and the peak. Offline only: these allocate
//! and are never called on the audio thread.

use std::f64::consts::PI;

use serde::Serialize;

use crate::dsp::{FS, db};
use crate::pitch::analysis::{Analysis, HOP};

/// The measurements of one clip.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Measurements {
    /// The median F0 of the voiced frames, in Hz; `None` when nothing is voiced.
    pub median_f0_hz: Option<f64>,
    /// The share of frames that are voiced, from 0 to 1.
    pub voiced: f64,
    /// F1 to F3 in Hz, from the average LPC envelope; `None` when the clip is too short or silent.
    pub formants_hz: Option<[f64; 3]>,
    /// The integrated loudness (ITU-R BS.1770-4, gated), in LUFS; `None` for silence.
    pub loudness_lufs: Option<f64>,
    /// The power-weighted mean frequency of the spectrum, in Hz.
    pub spectral_centroid_hz: f64,
    /// The highest sample, in dBFS.
    pub peak_dbfs: f64,
    /// The clip's length, in seconds.
    pub seconds: f64,
}

/// Measures `x`, mono at 48 kHz.
#[must_use]
#[allow(clippy::cast_precision_loss)] // sample counts
pub fn measure(x: &[f32]) -> Measurements {
    let (median_f0_hz, voiced) = f0(x);
    Measurements {
        median_f0_hz,
        voiced,
        formants_hz: formants(x),
        loudness_lufs: loudness(x),
        spectral_centroid_hz: centroid(x),
        peak_dbfs: db(x.iter().fold(0.0f64, |m, s| m.max(f64::from(s.abs())))),
        seconds: x.len() as f64 / FS,
    }
}

/// The median F0 of the voiced hops and the voiced share, from the shifter's own tracker.
#[allow(clippy::cast_precision_loss)]
fn f0(x: &[f32]) -> (Option<f64>, f64) {
    let mut analysis = Analysis::new(1);
    let mut periods = Vec::new();
    let mut hops = 0usize;
    for (n, s) in x.iter().enumerate() {
        analysis.push(f64::from(*s), false);
        // The tracker needs its first span of history before it says anything.
        if (n as i64) % HOP == 0 && n >= 1440 {
            hops += 1;
            let e = analysis.estimate();
            if e.voiced {
                periods.push(e.period);
            }
        }
    }
    if periods.is_empty() {
        return (None, 0.0);
    }
    periods.sort_by(f64::total_cmp);
    let median = periods[periods.len() / 2];
    (Some(FS / median), periods.len() as f64 / hops.max(1) as f64)
}

/// F1 to F3: the first three peaks above 200 Hz of the average LPC envelope (order 18, 2,048-sample Hann frames of
/// the pre-emphasised signal, 512 apart). Order 18 at 48 kHz resolves the formants without following the harmonics.
#[allow(clippy::cast_precision_loss)]
fn formants(x: &[f32]) -> Option<[f64; 3]> {
    const ORDER: usize = 18;
    const FRAME: usize = 2048;
    let grid: Vec<f64> = (0..=860).map(|i| 200.0 + 5.0 * f64::from(i)).collect();
    let mut envelope = vec![0.0f64; grid.len()];
    let window: Vec<f64> = (0..FRAME).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / FRAME as f64).cos()).collect();
    let mut frames = 0;
    let mut start = 0;
    while start + FRAME <= x.len() {
        let w: Vec<f64> = (0..FRAME)
            .map(|i| {
                let at = start + i;
                let pre = f64::from(x[at]) - if at > 0 { 0.97 * f64::from(x[at - 1]) } else { 0.0 };
                pre * window[i]
            })
            .collect();
        let r: Vec<f64> = (0..=ORDER).map(|k| (k..FRAME).map(|i| w[i] * w[i - k]).sum()).collect();
        start += 512;
        if r[0] <= 1e-12 {
            continue;
        }
        let a = levinson(&r);
        for (e, hz) in envelope.iter_mut().zip(&grid) {
            let w = 2.0 * PI * hz / FS;
            let (mut re, mut im) = (0.0, 0.0);
            for (k, c) in a.iter().enumerate() {
                let (s, co) = (w * k as f64).sin_cos();
                re += c * co;
                im -= c * s;
            }
            *e += 1.0 / (re * re + im * im);
        }
        frames += 1;
    }
    if frames == 0 {
        return None;
    }
    let mut peaks = [0.0; 3];
    let mut found = 0;
    for i in 1..grid.len() - 1 {
        if found < 3 && envelope[i] > envelope[i - 1] && envelope[i] >= envelope[i + 1] {
            peaks[found] = grid[i];
            found += 1;
        }
    }
    (found == 3).then_some(peaks)
}

/// Levinson-Durbin on `r`, any order.
fn levinson(r: &[f64]) -> Vec<f64> {
    let order = r.len() - 1;
    let mut a = vec![0.0f64; order + 1];
    a[0] = 1.0;
    let mut err = r[0];
    let mut previous = a.clone();
    for i in 1..=order {
        let acc: f64 = r[i] + (1..i).map(|j| a[j] * r[i - j]).sum::<f64>();
        let k = -acc / err;
        previous.copy_from_slice(&a);
        a[i] = k;
        for j in 1..i {
            a[j] = previous[j] + k * previous[i - j];
        }
        err *= 1.0 - k * k;
        if err <= 0.0 {
            break;
        }
    }
    a
}

/// A biquad over a whole signal, in place: `b0..b2`, `a1, a2` (direct form I).
fn biquad(x: &mut [f64], b: [f64; 3], a: [f64; 2]) {
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    for s in x.iter_mut() {
        let y = b[0] * *s + b[1] * x1 + b[2] * x2 - a[0] * y1 - a[1] * y2;
        x2 = x1;
        x1 = *s;
        y2 = y1;
        y1 = y;
        *s = y;
    }
}

/// The integrated loudness of ITU-R BS.1770-4 for one channel: K-weighting (its 48 kHz coefficients), 400 ms blocks
/// overlapping by 75%, the absolute gate at -70 LUFS and the relative gate 10 LU under the ungated mean.
#[allow(clippy::cast_precision_loss)]
fn loudness(x: &[f32]) -> Option<f64> {
    let mut k: Vec<f64> = x.iter().map(|s| f64::from(*s)).collect();
    biquad(
        &mut k,
        [1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85],
        [-1.690_659_293_182_41, 0.732_480_774_215_85],
    );
    biquad(&mut k, [1.0, -2.0, 1.0], [-1.990_047_454_833_98, 0.990_072_250_366_21]);
    let block = 19_200; // 400 ms
    let step = 4800; // 100 ms
    if k.len() < block {
        return None;
    }
    let powers: Vec<f64> = (0..=(k.len() - block) / step)
        .map(|i| k[i * step..i * step + block].iter().map(|v| v * v).sum::<f64>() / block as f64)
        .collect();
    let lufs = |z: f64| -0.691 + 10.0 * z.log10();
    let mean = |p: &[f64]| p.iter().sum::<f64>() / p.len() as f64;
    let loud: Vec<f64> = powers.iter().copied().filter(|z| *z > 0.0 && lufs(*z) > -70.0).collect();
    if loud.is_empty() {
        return None;
    }
    let relative = lufs(mean(&loud)) - 10.0;
    let gated: Vec<f64> = loud.into_iter().filter(|z| lufs(*z) > relative).collect();
    (!gated.is_empty()).then(|| lufs(mean(&gated)))
}

/// The power-weighted mean frequency over 2,048-sample Hann frames, 1,024 apart.
#[allow(clippy::cast_precision_loss)]
fn centroid(x: &[f32]) -> f64 {
    const N: usize = 2048;
    let window: Vec<f64> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / N as f64).cos()).collect();
    let (mut weighted, mut total) = (0.0, 0.0);
    let mut re = vec![0.0; N];
    let mut im = vec![0.0; N];
    let mut start = 0;
    while start + N <= x.len() {
        for i in 0..N {
            re[i] = f64::from(x[start + i]) * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        for bin in 1..N / 2 {
            let power = re[bin] * re[bin] + im[bin] * im[bin];
            weighted += power * bin as f64 * FS / N as f64;
            total += power;
        }
        start += N / 2;
    }
    if total > 0.0 { weighted / total } else { 0.0 }
}

/// An in-place radix-2 FFT; `re.len()` is a power of two.
#[allow(clippy::cast_precision_loss)]
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let (ws, wc) = (-2.0 * PI / len as f64).sin_cos();
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                (cr, ci) = (cr * wc - ci * ws, cr * ws + ci * wc);
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn sine(hz: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
        (0..(seconds * FS) as usize).map(|n| (amplitude * (2.0 * PI * hz * n as f64 / FS).sin()) as f32).collect()
    }

    #[test]
    fn a_full_scale_1_khz_sine_reads_minus_3_lufs() {
        // BS.1770's own check: a 0 dBFS 997 Hz sine reads -3.01 LKFS on one channel.
        let lufs = loudness(&sine(997.0, 1.0, 3.0)).unwrap();
        assert!((lufs + 3.01).abs() < 0.05, "{lufs}");
        assert!(loudness(&vec![0.0; 48_000]).is_none());
    }

    #[test]
    fn a_sine_has_its_own_frequency_as_its_centroid() {
        let c = centroid(&sine(2000.0, 0.5, 1.0));
        assert!((c - 2000.0).abs() < 30.0, "{c}");
    }
}
