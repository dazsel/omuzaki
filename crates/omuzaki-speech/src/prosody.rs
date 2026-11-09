//! Prosody reshaping: a voice's pitch, pitch range and tempo changed after synthesis, with its formants kept (#2683).
//!
//! TD-PSOLA (pitch-synchronous overlap-add, Moulines and Charpentier 1990). The voice is cut into two-period grains
//! centred on its pitch marks, and the grains are laid down again at the new period, at the time the tempo maps them
//! to. A grain carries the vocal tract's response to one glottal pulse, so moving grains closer together raises the
//! pitch without moving the formants. Resampling, the declared-rate shift the TTS host uses, moves both: +4 semitones
//! that way also makes the speaker's vocal tract 21 % shorter.
//!
//! - **Pitch:** a YIN track every 10 ms on the voice decimated to about 8 kHz, median-smoothed, with voiced runs under
//!   30 ms dropped.
//! - **Marks:** in voiced runs, one per period at the peak of the voice low-passed under 1 kHz, each searched within a
//!   quarter period of where the last one predicts; in unvoiced runs, every 5 ms.
//! - **Shape:** [`Shape::semitones`] moves every voiced grain; [`Shape::range`] scales each one's distance from the
//!   utterance's median pitch, in log frequency (2 swings the intonation twice as wide, 0 flattens it); [`Shape::tempo`]
//!   plays the marks faster or slower without changing the pitch.
//! - **Cost:** linear in the samples; about 1 % of real time on one core for 24 kHz speech (`tests/prosody.rs`).
//!
//! The identity shape returns the voice unchanged, sample for sample.

use std::f64::consts::PI;

/// The lowest and highest pitch tracked, Hz.
const F0_MIN: f64 = 60.0;
const F0_MAX: f64 = 500.0;
/// The pitch track's hop, s.
const TRACK_HOP_S: f64 = 0.01;
/// The rate the pitch is tracked at, about: the voice is decimated by a whole factor down to it.
const TRACK_RATE: u32 = 8_000;
/// YIN's dip threshold and the aperiodicity above which a frame is unvoiced.
const YIN_THRESHOLD: f64 = 0.15;
const YIN_VOICED: f64 = 0.35;
/// A frame quieter than this under the loudest frame is unvoiced, dB.
const SILENCE_DB: f64 = 40.0;
/// Voiced runs shorter than this many frames are dropped.
const MIN_VOICED_FRAMES: usize = 3;
/// The grain spacing in unvoiced runs, s.
const UNVOICED_HOP_S: f64 = 0.005;
/// The low-pass the marks are picked on, Hz.
const MARK_LOWPASS_HZ: f64 = 900.0;

/// How to reshape a voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    /// Pitch offset, semitones.
    pub semitones: f64,
    /// Intonation range: each voiced grain's distance from the median pitch, in log frequency, times this. 1 keeps
    /// it, 0 flattens the voice to a monotone, 2 doubles every rise and fall.
    pub range: f64,
    /// Tempo: 1.25 plays the voice in 80 % of the time, at the same pitch.
    pub tempo: f64,
}

impl Shape {
    /// The voice as it is.
    pub const IDENTITY: Self = Self { semitones: 0.0, range: 1.0, tempo: 1.0 };

    /// Whether this shape changes nothing.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// The range `expressiveness` asks for: 0.5 is the voice's own, each 0.5 either side halves or doubles it
    /// (`2^(2e − 1)`), so 0 is half as wide and 1 twice as wide. Out-of-range values are clamped to 0 to 1.
    #[must_use]
    pub fn range_for_expressiveness(expressiveness: f32) -> f64 {
        2f64.powf(2.0 * f64::from(expressiveness.clamp(0.0, 1.0)) - 1.0)
    }
}

/// One pitch mark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    /// Sample.
    pub at: usize,
    /// The grain's half length: the local period, or the unvoiced hop.
    pub period: usize,
    /// The pitch here, Hz; `None` when unvoiced.
    pub f0: Option<f64>,
}

/// The pitch every 10 ms of `pcm` at `rate`: Hz, or `None` where unvoiced.
#[must_use]
pub fn pitch_track(pcm: &[f32], rate: u32) -> Vec<Option<f64>> {
    let factor = (rate / TRACK_RATE).max(1) as usize;
    let fs = f64::from(rate) / factor as f64;
    // Decimate with a box filter: enough for the pitch, which sits far under the new Nyquist.
    let y: Vec<f64> =
        pcm.chunks(factor).map(|c| c.iter().map(|s| f64::from(*s)).sum::<f64>() / c.len() as f64).collect();
    let (min_lag, max_lag) = ((fs / F0_MAX).floor() as usize, (fs / F0_MIN).ceil() as usize);
    let window = max_lag * 2;
    let hop = (TRACK_HOP_S * fs).round() as usize;
    let frames = y.len() / hop.max(1) + 1;
    let energy: Vec<f64> = (0..frames)
        .map(|i| {
            let centre = i * hop;
            let lo = centre.saturating_sub(window / 2);
            let hi = (centre + window / 2).min(y.len());
            if hi <= lo { 0.0 } else { y[lo..hi].iter().map(|v| v * v).sum::<f64>() / (hi - lo) as f64 }
        })
        .collect();
    let loudest = energy.iter().copied().fold(0.0, f64::max);
    let floor = loudest * 10f64.powf(-SILENCE_DB / 10.0);
    let mut d = vec![0.0; max_lag + 1];
    let mut track: Vec<Option<f64>> = (0..frames)
        .map(|i| {
            if loudest <= 0.0 || energy[i] < floor {
                return None;
            }
            let start = (i * hop).saturating_sub(window / 2);
            if start + window + max_lag >= y.len() {
                return None;
            }
            yin(&y[start..=start + window + max_lag], window, min_lag, max_lag, &mut d).map(|lag| fs / lag)
        })
        .collect();
    smooth(&mut track);
    track
}

/// YIN's lag over `x` (window `w`, lags `min..=max`), or `None` when the frame isn't periodic enough.
#[allow(clippy::many_single_char_names)] // YIN's own names: the signal, its window, the lags and the difference
fn yin(x: &[f64], w: usize, min: usize, max: usize, d: &mut [f64]) -> Option<f64> {
    d[0] = 1.0;
    let mut running = 0.0;
    for tau in 1..=max {
        let raw: f64 = (0..w).map(|n| (x[n] - x[n + tau]).powi(2)).sum();
        running += raw;
        d[tau] = if running > 0.0 { raw * tau as f64 / running } else { 1.0 };
    }
    let mut best = None;
    let mut tau = min.max(2);
    while tau < max {
        if d[tau] < YIN_THRESHOLD {
            while tau + 1 < max && d[tau + 1] < d[tau] {
                tau += 1;
            }
            best = Some(tau);
            break;
        }
        tau += 1;
    }
    let tau = if let Some(t) = best {
        t
    } else {
        let t = (min.max(2)..max).min_by(|a, b| d[*a].total_cmp(&d[*b]))?;
        if d[t] > YIN_VOICED {
            return None;
        }
        t
    };
    // Parabolic interpolation around the dip.
    let (a, b, c) = (d[tau - 1], d[tau], d[tau + 1]);
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() > 1e-12 { 0.5 * (a - c) / denom } else { 0.0 };
    Some(tau as f64 + shift.clamp(-0.5, 0.5))
}

/// A 3-frame median over voiced neighbours, then voiced runs shorter than [`MIN_VOICED_FRAMES`] dropped.
fn smooth(track: &mut [Option<f64>]) {
    let raw = track.to_vec();
    for i in 1..raw.len().saturating_sub(1) {
        if let (Some(a), Some(b), Some(c)) = (raw[i - 1], raw[i], raw[i + 1]) {
            let mut v = [a, b, c];
            v.sort_by(f64::total_cmp);
            track[i] = Some(v[1]);
        }
    }
    let mut i = 0;
    while i < track.len() {
        if track[i].is_none() {
            i += 1;
            continue;
        }
        let start = i;
        while i < track.len() && track[i].is_some() {
            i += 1;
        }
        if i - start < MIN_VOICED_FRAMES {
            track[start..i].iter_mut().for_each(|f| *f = None);
        }
    }
}

/// The pitch at sample `n`, from the 10 ms track: linear between two voiced frames, `None` next to an unvoiced one.
fn pitch_at(track: &[Option<f64>], n: usize, rate: u32) -> Option<f64> {
    let pos = n as f64 / (TRACK_HOP_S * f64::from(rate));
    let i = pos.floor() as usize;
    match (track.get(i).copied().flatten(), track.get(i + 1).copied().flatten()) {
        (Some(a), Some(b)) => Some(a + (b - a) * (pos - i as f64)),
        (Some(a), None) if pos - (i as f64) < 0.5 => Some(a),
        (None, Some(b)) if pos - (i as f64) >= 0.5 => Some(b),
        _ => None,
    }
}

/// The pitch marks of `pcm` at `rate`.
#[must_use]
pub fn marks(pcm: &[f32], rate: u32) -> Vec<Mark> {
    let track = pitch_track(pcm, rate);
    let fs = f64::from(rate);
    let low = lowpass(pcm, rate, MARK_LOWPASS_HZ);
    let unvoiced = (UNVOICED_HOP_S * fs).round().max(1.0) as usize;
    let mut out: Vec<Mark> = Vec::new();
    let mut n = 0usize;
    while n < pcm.len() {
        let Some(f0) = pitch_at(&track, n, rate) else {
            out.push(Mark { at: n, period: unvoiced, f0: None });
            n += unvoiced;
            continue;
        };
        let period = (fs / f0).round().max(2.0) as usize;
        // Search a quarter period either side of where the last voiced mark predicts; a whole period from an
        // unvoiced start.
        let (lo, hi) = match out.last() {
            Some(last) if last.f0.is_some() => (n.saturating_sub(period / 4), n + period / 4),
            _ => (n, n + period),
        };
        let hi = hi.min(pcm.len().saturating_sub(1));
        let at = (lo..=hi).max_by(|a, b| low[*a].total_cmp(&low[*b])).unwrap_or(n);
        let at = match out.last() {
            Some(last) if at <= last.at => n,
            _ => at,
        };
        out.push(Mark { at, period, f0: Some(f0) });
        n = at + period;
    }
    out
}

/// `pcm` through a zero-phase one-pole low-pass at `hz` (forward, then backward).
fn lowpass(pcm: &[f32], rate: u32, hz: f64) -> Vec<f64> {
    let a = (-2.0 * PI * hz / f64::from(rate)).exp();
    let mut y: Vec<f64> = Vec::with_capacity(pcm.len());
    let mut s = 0.0;
    for x in pcm {
        s = (1.0 - a) * f64::from(*x) + a * s;
        y.push(s);
    }
    s = 0.0;
    for v in y.iter_mut().rev() {
        s = (1.0 - a) * *v + a * s;
        *v = s;
    }
    y
}

/// `pcm` at `rate` reshaped by `shape`. The identity shape returns it unchanged.
#[must_use]
pub fn reshape(pcm: &[f32], rate: u32, shape: &Shape) -> Vec<f32> {
    if shape.is_identity() || pcm.is_empty() {
        return pcm.to_vec();
    }
    let marks = marks(pcm, rate);
    reshape_with(pcm, rate, &marks, shape)
}

/// [`reshape`] on marks already found.
#[must_use]
pub fn reshape_with(pcm: &[f32], rate: u32, marks: &[Mark], shape: &Shape) -> Vec<f32> {
    let tempo = if shape.tempo.is_finite() && shape.tempo > 0.0 { shape.tempo } else { 1.0 };
    let out_len = (pcm.len() as f64 / tempo).round() as usize;
    if marks.is_empty() || out_len == 0 {
        return vec![0.0; out_len];
    }
    let mut voiced: Vec<f64> = marks.iter().filter_map(|m| m.f0).map(f64::ln).collect();
    voiced.sort_by(f64::total_cmp);
    let centre = voiced.get(voiced.len() / 2).copied().unwrap_or(0.0);
    let shift = shape.semitones / 12.0 * std::f64::consts::LN_2;
    let fs = f64::from(rate);
    let mut out = vec![0.0f64; out_len];
    let mut weight = vec![0.0f64; out_len];
    let mut t = marks[0].at as f64 / tempo;
    let mut k = 0usize;
    while t < out_len as f64 {
        // The analysis mark nearest the time this output instant maps to.
        let source = t * tempo;
        while k + 1 < marks.len() && (marks[k + 1].at as f64 - source).abs() <= (marks[k].at as f64 - source).abs() {
            k += 1;
        }
        let mark = marks[k];
        let step = match mark.f0 {
            Some(f0) => {
                let target = (centre + shape.range * (f0.ln() - centre) + shift).exp();
                (fs / target).max(2.0)
            }
            None => mark.period as f64,
        };
        let half = isize::try_from(mark.period).unwrap_or(isize::MAX / 2);
        let centre_out = t.round() as isize;
        for j in -half..=half {
            let (Some(src), Some(dst)) = (offset(mark.at, j, pcm.len()), offset_signed(centre_out, j, out_len)) else {
                continue;
            };
            let w = 0.5 + 0.5 * (PI * j as f64 / (half as f64 + 1.0)).cos();
            out[dst] += w * f64::from(pcm[src]);
            weight[dst] += w;
        }
        t += step;
    }
    out.iter().zip(&weight).map(|(v, w)| (v / w.max(1.0)) as f32).collect()
}

fn offset(at: usize, j: isize, len: usize) -> Option<usize> {
    at.checked_add_signed(j).filter(|i| *i < len)
}

fn offset_signed(at: isize, j: isize, len: usize) -> Option<usize> {
    usize::try_from(at + j).ok().filter(|i| *i < len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f0: impl Fn(f64) -> f64, seconds: f64, rate: u32) -> Vec<f32> {
        let fs = f64::from(rate);
        let mut phase = 0.0;
        (0..(seconds * fs) as usize)
            .map(|n| {
                phase += f0(n as f64 / fs) / fs;
                (0.4 * (2.0 * PI * phase).sin() + 0.2 * (4.0 * PI * phase).sin()) as f32
            })
            .collect()
    }

    fn mean_pitch(pcm: &[f32], rate: u32) -> f64 {
        let v: Vec<f64> = pitch_track(pcm, rate).into_iter().flatten().collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    }

    #[test]
    fn the_track_reads_a_steady_tone() {
        let p = mean_pitch(&tone(|_| 180.0, 1.0, 24_000), 24_000);
        assert!((p - 180.0).abs() < 2.0, "{p}");
    }

    #[test]
    fn the_identity_changes_nothing() {
        let x = tone(|_| 150.0, 0.5, 24_000);
        assert_eq!(reshape(&x, 24_000, &Shape::IDENTITY), x);
    }

    #[test]
    fn semitones_move_the_pitch_and_tempo_keeps_it() {
        let x = tone(|_| 150.0, 1.0, 24_000);
        let up = reshape(&x, 24_000, &Shape { semitones: 4.0, ..Shape::IDENTITY });
        let p = mean_pitch(&up, 24_000);
        assert!((p / 150.0 - 2f64.powf(4.0 / 12.0)).abs() < 0.02, "{p}");
        assert_eq!(up.len(), x.len());
        let fast = reshape(&x, 24_000, &Shape { tempo: 1.25, ..Shape::IDENTITY });
        assert_eq!(fast.len(), (x.len() as f64 / 1.25).round() as usize);
        let p = mean_pitch(&fast, 24_000);
        assert!((p - 150.0).abs() < 3.0, "{p}");
    }

    #[test]
    fn range_widens_a_glide_about_its_median() {
        let x = tone(|t| 120.0 * 1.5f64.powf(t), 1.0, 24_000);
        let wide = reshape(&x, 24_000, &Shape { range: 2.0, ..Shape::IDENTITY });
        let cents = |pcm: &[f32]| -> f64 {
            let v: Vec<f64> = pitch_track(pcm, 24_000).into_iter().flatten().map(|f| 1200.0 * f.log2()).collect();
            let m = v.iter().sum::<f64>() / v.len() as f64;
            (v.iter().map(|c| (c - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
        };
        let ratio = cents(&wide) / cents(&x);
        assert!((ratio - 2.0).abs() < 0.2, "{ratio}");
    }

    #[test]
    fn expressiveness_maps_to_range() {
        assert!((Shape::range_for_expressiveness(0.5) - 1.0).abs() < 1e-12);
        assert!((Shape::range_for_expressiveness(1.0) - 2.0).abs() < 1e-12);
        assert!((Shape::range_for_expressiveness(0.0) - 0.5).abs() < 1e-12);
    }
}
