//! Measured voice parity: our audio against a reference recording of the same words, aligned frame by frame and
//! scored as numbers, never by ear (the reference loop in CLAUDE.md, for sound).
//!
//! Both signals (16 kHz mono) are cut into 25 ms frames every 10 ms. Each frame gets a 40-band log-mel spectrum, its
//! mel-frequency cepstrum (c1 to c13, the spectral envelope without loudness) and a YIN pitch. Dynamic time warping
//! over the cepstra lines our frames up with the reference's (inside a band around the diagonal, after trimming the
//! silence at both ends), and every measure is read along that path:
//!
//! | Measure | What it says | Unit |
//! |---|---|---|
//! | `mcd_db` | mel-cepstral distortion: how far the timbre and the vowels are | dB |
//! | `spectral_db` | log-mel distance, loudness taken out: the whole spectrum's shape | dB |
//! | `level_db` | our speech level against the reference's | dB |
//! | `f0_bias_cents` | the mean pitch offset over frames voiced in both | cents |
//! | `f0_rmse_cents` | the pitch error frame by frame, the offset included | cents |
//! | `f0_shape_cents` | the same with the offset taken out: the intonation's shape | cents |
//! | `voicing_error` | the share of speech frames where one is voiced and the other isn't | 0 to 1 |
//! | `duration_ratio` | our speech span over the reference's | ratio |
//! | `timing_rms_ms` | how far the path strays from an even tempo: the rhythm | ms |
//! | `segments` | each reference phrase's start and end, and where it falls in ours | ms |
//!
//! Exact sample parity is meaningless for two recordings (the same person never says a line the same way twice), so
//! the bar is the **two-takes floor**: our distance to the reference measured no larger than the distance between two
//! takes of the reference speaker saying the same line, measure by measure ([`Parity::within`]).
//!
//! The `voice.compare` op ([`ops`]) reads the files, runs this, and answers in the shape every track's parity op
//! shares. [`overlay`] draws the comparison: our spectrogram warped onto the reference's time, the reference's, their
//! difference, and both pitch tracks. Mel-cepstral distortion here is computed on MFCCs (DCT of the log-mel energies,
//! natural log of the amplitude), the common approximation of the SPTK mel-cepstrum: compare it with floors measured
//! the same way, not with published numbers from another front end.

// DSP and dynamic programming read best in their textbook names: a and b, i and j, n and m, x, y, re and im.
#![allow(clippy::many_single_char_names)]

use std::f64::consts::PI;

pub mod ops;

/// The analysis rate. Callers resample to it first.
pub const RATE: u32 = 16_000;
/// Samples per frame step: 10 ms.
pub const HOP: usize = 160;
/// Samples per analysis window: 25 ms.
pub const WINDOW: usize = 400;
/// The FFT size.
const FFT: usize = 512;
/// Mel bands.
pub const MEL_BANDS: usize = 40;
/// Cepstral coefficients compared (c1 to c13).
pub const CEPSTRA: usize = 13;
/// The pitch window: 40 ms, two periods of the lowest pitch.
const PITCH_WINDOW: usize = 640;
/// The pitch range, Hz.
const F0_MIN: f64 = 60.0;
const F0_MAX: f64 = 500.0;
/// YIN's aperiodicity threshold, and the most a voiced frame may have.
const YIN_THRESHOLD: f64 = 0.15;
const YIN_VOICED: f64 = 0.3;
/// Frames this far under the loudest are silence, dB.
const SILENCE_DB: f64 = 40.0;
/// The shortest pause that splits phrases, frames.
const PAUSE_FRAMES: usize = 12;
/// The DTW band's half-width: this share of the longer signal, at least `BAND_MIN` frames.
const BAND_SHARE: f64 = 0.2;
const BAND_MIN: usize = 40;
/// The longest signal compared: 3 minutes.
pub const MAX_SECONDS: usize = 180;

/// Why two signals can't be compared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParityError {
    /// One side has no speech at all.
    #[error("the {0} has no speech: nothing above the silence floor")]
    Silent(&'static str),
    /// One side is longer than [`MAX_SECONDS`].
    #[error("the {0} is longer than {MAX_SECONDS} s; compare it in parts")]
    TooLong(&'static str),
}

/// One 10 ms frame's features.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Log-mel energies, natural log of power.
    pub mel: [f64; MEL_BANDS],
    /// Mel cepstrum c1 to c13 (natural log of amplitude).
    pub cepstrum: [f64; CEPSTRA],
    /// Frame energy, dB.
    pub energy_db: f64,
    /// Pitch, Hz, when voiced.
    pub f0: Option<f64>,
    /// Above the silence floor.
    pub speech: bool,
}

/// A signal's frames.
#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    /// One per 10 ms.
    pub frames: Vec<Frame>,
}

impl Features {
    /// The first and one past the last speech frame.
    #[must_use]
    pub fn speech_span(&self) -> Option<(usize, usize)> {
        let first = self.frames.iter().position(|f| f.speech)?;
        let last = self.frames.iter().rposition(|f| f.speech)?;
        Some((first, last + 1))
    }
}

/// Analyses 16 kHz mono samples.
#[must_use]
pub fn analyse(samples: &[f32]) -> Features {
    let bank = mel_bank();
    let window: Vec<f64> = (0..WINDOW).map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / WINDOW as f64).cos()).collect();
    let count = samples.len().div_ceil(HOP);
    let mut re = vec![0.0; FFT];
    let mut im = vec![0.0; FFT];
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        let center = i * HOP + HOP / 2;
        let at =
            |n: isize| -> f64 { usize::try_from(n).ok().and_then(|n| samples.get(n)).map_or(0.0, |s| f64::from(*s)) };
        let start = center.cast_signed() - (WINDOW / 2).cast_signed();
        let mut power = 0.0;
        for n in 0..FFT {
            re[n] = if n < WINDOW { at(start + n.cast_signed()) * window[n] } else { 0.0 };
            im[n] = 0.0;
            if n < WINDOW {
                power += at(start + n.cast_signed()).powi(2);
            }
        }
        fft(&mut re, &mut im);
        let spectrum: Vec<f64> = (0..=FFT / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect();
        let mut mel = [0.0; MEL_BANDS];
        for (band, weights) in mel.iter_mut().zip(&bank) {
            let e: f64 = weights.iter().map(|&(k, w)| w * spectrum[k]).sum();
            *band = e.max(1e-10).ln();
        }
        let cepstrum = dct(&mel);
        let pitch_start = center.cast_signed() - (PITCH_WINDOW / 2).cast_signed();
        let pitch: Vec<f64> = (0..PITCH_WINDOW).map(|n| at(pitch_start + n.cast_signed())).collect();
        frames.push(Frame {
            mel,
            cepstrum,
            energy_db: 10.0 * (power / WINDOW as f64).max(1e-12).log10(),
            f0: yin(&pitch),
            speech: false,
        });
    }
    let loudest = frames.iter().map(|f| f.energy_db).fold(f64::NEG_INFINITY, f64::max);
    for frame in &mut frames {
        frame.speech = frame.energy_db > loudest - SILENCE_DB && frame.energy_db > -70.0;
        if !frame.speech {
            frame.f0 = None;
        }
    }
    Features { frames }
}

/// The reference's phrase `index`, and where it falls in ours.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Segment {
    /// The phrase's start and end in the reference, ms from its first speech.
    pub reference_ms: [f64; 2],
    /// Where the path puts them in ours, ms from our first speech.
    pub ours_ms: [f64; 2],
    /// Ours minus the reference, at the start and the end, ms.
    pub offset_ms: [f64; 2],
    /// Mel-cepstral distortion inside the phrase, dB.
    pub mcd_db: f64,
    /// Pitch error inside the phrase, cents.
    pub f0_rmse_cents: f64,
    /// Voicing mismatches inside the phrase, as a share.
    pub voicing_error: f64,
    /// Frames on the path inside the phrase that both call speech.
    pub frames: usize,
}

/// The measures of one comparison. See the module table.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Measures {
    /// Mel-cepstral distortion, dB.
    pub mcd_db: f64,
    /// Log-mel distance with the level taken out, dB.
    pub spectral_db: f64,
    /// Our speech level over the reference's, dB.
    pub level_db: f64,
    /// Mean pitch offset, cents (positive: ours is higher).
    pub f0_bias_cents: f64,
    /// Pitch error, cents.
    pub f0_rmse_cents: f64,
    /// Pitch error with the offset taken out, cents.
    pub f0_shape_cents: f64,
    /// Share of speech frames with a voicing mismatch.
    pub voicing_error: f64,
    /// Our speech span over the reference's.
    pub duration_ratio: f64,
    /// The path's RMS distance from an even tempo, ms.
    pub timing_rms_ms: f64,
    /// Frames on the path that both call speech.
    pub frames: usize,
}

/// A comparison: the measures, the reference's phrases, and what [`overlay`] draws.
#[derive(Debug, Clone)]
pub struct Parity {
    /// The numbers.
    pub measures: Measures,
    /// Each reference phrase's timing.
    pub segments: Vec<Segment>,
    /// For each reference frame in its speech span, our frames' mean on the path (warped onto the reference).
    warped: Vec<Frame>,
    /// The reference's frames in its speech span.
    reference: Vec<Frame>,
}

/// How a measure compares with its floor.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Check {
    /// The measure's name, as in [`Measures`].
    pub measure: &'static str,
    /// Ours against the reference.
    pub value: f64,
    /// Two takes of the reference against each other.
    pub floor: f64,
    /// Whether ours is within the floor (for a ratio: as close to 1).
    pub within: bool,
}

impl Parity {
    /// Each measure against the same measure between two takes of the reference. Parity holds when every one is
    /// within.
    #[must_use]
    pub fn within(&self, floor: &Measures) -> Vec<Check> {
        let ours = &self.measures;
        let pairs: [(&'static str, f64, f64); 8] = [
            ("mcd_db", ours.mcd_db, floor.mcd_db),
            ("spectral_db", ours.spectral_db, floor.spectral_db),
            ("level_db", ours.level_db.abs(), floor.level_db.abs()),
            ("f0_bias_cents", ours.f0_bias_cents.abs(), floor.f0_bias_cents.abs()),
            ("f0_shape_cents", ours.f0_shape_cents, floor.f0_shape_cents),
            ("voicing_error", ours.voicing_error, floor.voicing_error),
            ("duration_ratio", (ours.duration_ratio.ln()).abs(), (floor.duration_ratio.ln()).abs()),
            ("timing_rms_ms", ours.timing_rms_ms, floor.timing_rms_ms),
        ];
        pairs
            .into_iter()
            .map(|(measure, value, floor)| Check { measure, value, floor, within: value <= floor + 1e-9 })
            .collect()
    }
}

/// Compares `ours` against `reference` (both from [`analyse`]).
///
/// # Errors
/// [`ParityError::Silent`] when either has no speech; [`ParityError::TooLong`] past [`MAX_SECONDS`].
#[allow(clippy::too_many_lines, reason = "one pass along the path: every measure reads the same aligned frames")]
pub fn compare(ours: &Features, reference: &Features) -> Result<Parity, ParityError> {
    let limit = MAX_SECONDS * 100;
    if ours.frames.len() > limit {
        return Err(ParityError::TooLong("our audio"));
    }
    if reference.frames.len() > limit {
        return Err(ParityError::TooLong("reference"));
    }
    let (o0, o1) = ours.speech_span().ok_or(ParityError::Silent("our audio"))?;
    let (r0, r1) = reference.speech_span().ok_or(ParityError::Silent("reference"))?;
    let a = &ours.frames[o0..o1];
    let b = &reference.frames[r0..r1];
    let path = dtw(a, b);

    // Our frames on the path for each reference frame.
    let mut mapped: Vec<Vec<usize>> = vec![Vec::new(); b.len()];
    for &(i, j) in &path {
        mapped[j].push(i);
    }

    let level = |frames: &[Frame]| {
        let speech: Vec<f64> = frames.iter().filter(|f| f.speech).map(|f| f.energy_db).collect();
        speech.iter().sum::<f64>() / speech.len().max(1) as f64
    };
    let level_db = level(a) - level(b);
    let gain_ln = level_db / 10.0 * std::f64::consts::LN_10;

    let spans = phrases(b);
    let mut phrase_of = vec![None; b.len()];
    for (k, &(s, e)) in spans.iter().enumerate() {
        phrase_of[s..e].fill(Some(k));
    }
    // Per phrase: MCD sum, squared cents, pitched frames, mismatches, frames.
    let mut per = vec![(0.0, 0.0, 0usize, 0usize, 0usize); spans.len()];
    let (mut mcd, mut spectral, mut both) = (0.0, 0.0, 0usize);
    let mut cents = Vec::new();
    let mut mismatched = 0usize;
    for &(i, j) in &path {
        let (x, y) = (&a[i], &b[j]);
        if !(x.speech && y.speech) {
            continue;
        }
        both += 1;
        let d2: f64 = x.cepstrum.iter().zip(&y.cepstrum).map(|(p, q)| (p - q).powi(2)).sum();
        let frame_mcd = 10.0 / std::f64::consts::LN_10 * (2.0 * d2).sqrt();
        mcd += frame_mcd;
        let s2: f64 = x.mel.iter().zip(&y.mel).map(|(p, q)| (p - gain_ln - q).powi(2)).sum::<f64>() / MEL_BANDS as f64;
        spectral += 10.0 / std::f64::consts::LN_10 * s2.sqrt();
        let phrase = phrase_of[j].map(|k| &mut per[k]);
        let (mut cent, mut mismatch) = (None, false);
        match (x.f0, y.f0) {
            (Some(p), Some(q)) => cent = Some(1200.0 * (p / q).log2()),
            (None, None) => {}
            _ => mismatch = true,
        }
        if let Some(c) = cent {
            cents.push(c);
        }
        mismatched += usize::from(mismatch);
        if let Some(acc) = phrase {
            acc.0 += frame_mcd;
            if let Some(c) = cent {
                acc.1 += c * c;
                acc.2 += 1;
            }
            acc.3 += usize::from(mismatch);
            acc.4 += 1;
        }
    }
    let speech_frames = both;
    let n = both.max(1) as f64;
    let bias = cents.iter().sum::<f64>() / cents.len().max(1) as f64;
    let rmse = (cents.iter().map(|c| c * c).sum::<f64>() / cents.len().max(1) as f64).sqrt();
    let shape = (cents.iter().map(|c| (c - bias).powi(2)).sum::<f64>() / cents.len().max(1) as f64).sqrt();

    // Rhythm: where the path puts each reference frame, against the straight line between the ends.
    let slope = (a.len() - 1).max(1) as f64 / (b.len() - 1).max(1) as f64;
    let mut stray = 0.0;
    for (j, is) in mapped.iter().enumerate() {
        let mean = is.iter().sum::<usize>() as f64 / is.len().max(1) as f64;
        stray += (mean - j as f64 * slope).powi(2);
    }
    let timing_rms_ms = (stray / b.len() as f64).sqrt() * 10.0;

    let segments = spans
        .iter()
        .zip(&per)
        .map(|(&(s, e), &(seg_mcd, sq, pitched, miss, frames))| {
            let start = mapped[s].first().copied().unwrap_or(0) as f64 * 10.0;
            let end = (mapped[e - 1].last().copied().unwrap_or(0) + 1) as f64 * 10.0;
            let (rs, re) = (s as f64 * 10.0, e as f64 * 10.0);
            Segment {
                reference_ms: [rs, re],
                ours_ms: [start, end],
                offset_ms: [start - rs, end - re],
                mcd_db: seg_mcd / frames.max(1) as f64,
                f0_rmse_cents: (sq / pitched.max(1) as f64).sqrt(),
                voicing_error: miss as f64 / frames.max(1) as f64,
                frames,
            }
        })
        .collect();

    let warped = mapped
        .iter()
        .map(|is| {
            let k = is.len().max(1) as f64;
            let mut mel = [0.0; MEL_BANDS];
            for &i in is {
                for (m, v) in mel.iter_mut().zip(&a[i].mel) {
                    *m += v / k;
                }
            }
            let pitches: Vec<f64> = is.iter().filter_map(|&i| a[i].f0).collect();
            let f0 = (pitches.len() * 2 > is.len()).then(|| pitches.iter().sum::<f64>() / pitches.len() as f64);
            Frame { mel, cepstrum: [0.0; CEPSTRA], energy_db: 0.0, f0, speech: true }
        })
        .collect();

    Ok(Parity {
        measures: Measures {
            mcd_db: mcd / n,
            spectral_db: spectral / n,
            level_db,
            f0_bias_cents: bias,
            f0_rmse_cents: rmse,
            f0_shape_cents: shape,
            voicing_error: mismatched as f64 / speech_frames.max(1) as f64,
            duration_ratio: a.len() as f64 / b.len() as f64,
            timing_rms_ms,
            frames: both,
        },
        segments,
        warped,
        reference: b.to_vec(),
    })
}

/// An RGB image, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// `width * height * 3` bytes.
    pub rgb: Vec<u8>,
}

/// Rows per mel band in the overlay.
const BAND_ROWS: usize = 3;
/// The pitch panel's height.
const PITCH_ROWS: usize = 120;
/// The gap between panels.
const GAP: usize = 4;

/// Draws a comparison, top to bottom: our spectrogram warped onto the reference's time, the reference's, the
/// difference (red where ours is louder, blue where it is quieter, level taken out), and the pitch tracks (white the
/// reference, cyan ours, 60 to 500 Hz on a log scale). One column per 10 ms of the reference's speech.
#[must_use]
pub fn overlay(parity: &Parity) -> Image {
    let width = parity.reference.len().max(1);
    let panel = MEL_BANDS * BAND_ROWS;
    let height = 3 * panel + PITCH_ROWS + 3 * GAP;
    let mut rgb = vec![24u8; width * height * 3];
    let mut put = |x: usize, y: usize, c: [u8; 3]| {
        let at = (y * width + x) * 3;
        rgb[at..at + 3].copy_from_slice(&c);
    };
    let span = |frames: &[Frame]| {
        let top = frames.iter().flat_map(|f| f.mel).fold(f64::NEG_INFINITY, f64::max);
        (top - 80.0 / 10.0 * std::f64::consts::LN_10, top)
    };
    let gain_ln = parity.measures.level_db / 10.0 * std::f64::consts::LN_10;
    let (lo_w, hi_w) = span(&parity.warped);
    let (lo_r, hi_r) = span(&parity.reference);
    for x in 0..width {
        let (w, r) = (&parity.warped[x], &parity.reference[x]);
        for band in 0..MEL_BANDS {
            for k in 0..BAND_ROWS {
                // Low frequencies at the bottom of each panel.
                let row = (MEL_BANDS - 1 - band) * BAND_ROWS + k;
                put(x, row, heat((w.mel[band] - lo_w) / (hi_w - lo_w)));
                put(x, panel + GAP + row, heat((r.mel[band] - lo_r) / (hi_r - lo_r)));
                let diff = (w.mel[band] - gain_ln - r.mel[band]) * 10.0 / std::f64::consts::LN_10;
                put(x, 2 * (panel + GAP) + row, diverging(diff / 20.0));
            }
        }
        let pitch_top = 3 * (panel + GAP);
        let y_of = |f0: f64| {
            let t = ((f0 / F0_MIN).ln() / (F0_MAX / F0_MIN).ln()).clamp(0.0, 1.0);
            pitch_top + ((1.0 - t) * (PITCH_ROWS - 1) as f64).round() as usize
        };
        if let Some(f0) = r.f0 {
            put(x, y_of(f0), [255, 255, 255]);
        }
        if let Some(f0) = w.f0 {
            put(x, y_of(f0), [0, 220, 255]);
        }
    }
    Image { width: u32::try_from(width).unwrap_or(u32::MAX), height: u32::try_from(height).unwrap_or(u32::MAX), rgb }
}

/// Dark to bright, through purple and orange.
fn heat(t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let channel = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [channel(1.6 * t), channel(1.8 * t - 0.6), channel(0.6 * t + 0.6 * (1.0 - t) * t * 2.0)]
}

/// Blue for negative, red for positive, dark at zero.
fn diverging(t: f64) -> [u8; 3] {
    let t = t.clamp(-1.0, 1.0);
    let v = (t.abs() * 255.0).round() as u8;
    if t >= 0.0 { [v, v / 5, v / 5] } else { [v / 5, v / 3, v] }
}

/// The reference's phrases: speech runs split at pauses of at least [`PAUSE_FRAMES`].
fn phrases(frames: &[Frame]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    let mut quiet = 0;
    for (i, f) in frames.iter().enumerate() {
        if f.speech {
            if start.is_none() {
                start = Some(i);
            }
            quiet = 0;
        } else if let Some(s) = start {
            quiet += 1;
            if quiet == PAUSE_FRAMES {
                out.push((s, i + 1 - PAUSE_FRAMES));
                start = None;
            }
        }
    }
    if let Some(s) = start {
        out.push((s, frames.len() - quiet));
    }
    out
}

/// Dynamic time warping over the cepstra, inside a band around the diagonal. Returns the path from (0, 0) to the
/// last frames, as (ours, reference) pairs.
fn dtw(a: &[Frame], b: &[Frame]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let radius = ((n.max(m) as f64 * BAND_SHARE) as usize).max(BAND_MIN);
    let center = |i: usize| if n <= 1 { 0 } else { (i as f64 * (m - 1) as f64 / (n - 1) as f64).round() as usize };
    let lo = |i: usize| center(i).saturating_sub(radius);
    let hi = |i: usize| (center(i) + radius).min(m - 1);
    let cost = |i: usize, j: usize| -> f64 {
        a[i].cepstrum.iter().zip(&b[j].cepstrum).map(|(p, q)| (p - q).powi(2)).sum::<f64>().sqrt()
    };
    // Directions in the band only: 0 diagonal, 1 from above (i - 1), 2 from the left (j - 1).
    let mut steps: Vec<Vec<u8>> = Vec::with_capacity(n);
    let mut prev = vec![f64::INFINITY; m];
    let mut row = vec![f64::INFINITY; m];
    for i in 0..n {
        row.fill(f64::INFINITY);
        let (l, h) = (lo(i), hi(i));
        let mut dirs = vec![0u8; h + 1 - l];
        for j in l..=h {
            let c = cost(i, j);
            let (best, dir) = if i == 0 && j == 0 {
                (0.0, 0)
            } else {
                let diag = if i > 0 && j > 0 { prev[j - 1] } else { f64::INFINITY };
                let up = if i > 0 { prev[j] } else { f64::INFINITY };
                let left = if j > 0 { row[j - 1] } else { f64::INFINITY };
                if diag <= up && diag <= left {
                    (diag, 0)
                } else if up <= left {
                    (up, 1)
                } else {
                    (left, 2)
                }
            };
            row[j] = best + c;
            dirs[j - l] = dir;
        }
        steps.push(dirs);
        std::mem::swap(&mut prev, &mut row);
    }
    let (mut i, mut j) = (n - 1, m - 1);
    let mut path = vec![(i, j)];
    while i > 0 || j > 0 {
        let dir = j.checked_sub(lo(i)).and_then(|k| steps[i].get(k)).copied().unwrap_or(0);
        match dir {
            0 if i > 0 && j > 0 => {
                i -= 1;
                j -= 1;
            }
            1 if i > 0 => i -= 1,
            _ if j > 0 => j -= 1,
            _ => i -= 1,
        }
        path.push((i, j));
    }
    path.reverse();
    path
}

/// Triangular mel filters, 60 Hz to 7.6 kHz: per band, the FFT bins and their weights.
fn mel_bank() -> Vec<Vec<(usize, f64)>> {
    let mel = |hz: f64| 2595.0 * (1.0 + hz / 700.0).log10();
    let hz = |mel: f64| 700.0 * (10f64.powf(mel / 2595.0) - 1.0);
    let (lo, hi) = (mel(60.0), mel(7600.0));
    let edges: Vec<f64> = (0..MEL_BANDS + 2).map(|k| hz(lo + (hi - lo) * k as f64 / (MEL_BANDS + 1) as f64)).collect();
    let bin_hz = f64::from(RATE) / FFT as f64;
    (0..MEL_BANDS)
        .map(|b| {
            let (left, mid, right) = (edges[b], edges[b + 1], edges[b + 2]);
            (0..=FFT / 2)
                .filter_map(|k| {
                    let f = k as f64 * bin_hz;
                    let w = if f > left && f <= mid {
                        (f - left) / (mid - left)
                    } else if f > mid && f < right {
                        (right - f) / (right - mid)
                    } else {
                        0.0
                    };
                    (w > 0.0).then_some((k, w))
                })
                .collect()
        })
        .collect()
}

/// The mel cepstrum c1 to c13 of log-mel powers, on SPTK's scale: `log|H(w)| = c0 + 2 sum c_k cos(k w)`, so
/// `c_k = 1/N sum log|H| cos(k w_i)` over the bands (an orthonormal DCT would be sqrt(N/2) times larger, and the MCD
/// with it, 4.5 times larger at 40 bands, than the values published for speech).
fn dct(log_power: &[f64; MEL_BANDS]) -> [f64; CEPSTRA] {
    let n = MEL_BANDS as f64;
    let mut out = [0.0; CEPSTRA];
    for (k, c) in out.iter_mut().enumerate() {
        let q = (k + 1) as f64;
        *c = (1.0 / n)
            * log_power.iter().enumerate().map(|(i, v)| 0.5 * v * (PI * q * (i as f64 + 0.5) / n).cos()).sum::<f64>();
    }
    out
}

/// YIN's pitch of one window, Hz; `None` when it isn't periodic enough.
fn yin(x: &[f64]) -> Option<f64> {
    let rate = f64::from(RATE);
    let (min_lag, max_lag) = ((rate / F0_MAX) as usize, (rate / F0_MIN) as usize);
    let w = x.len() - max_lag - 1;
    let mut d = vec![0.0; max_lag + 2];
    for (tau, slot) in d.iter_mut().enumerate().take(max_lag + 2).skip(1) {
        *slot = (0..w).map(|n| (x[n] - x[n + tau]).powi(2)).sum();
    }
    // Cumulative mean normalised difference.
    let mut sum = 0.0;
    let mut cmnd = vec![1.0; max_lag + 2];
    for tau in 1..max_lag + 2 {
        sum += d[tau];
        cmnd[tau] = if sum > 0.0 { d[tau] * tau as f64 / sum } else { 1.0 };
    }
    let mut tau = (min_lag..=max_lag).find(|&t| cmnd[t] < YIN_THRESHOLD)?;
    while tau < max_lag && cmnd[tau + 1] < cmnd[tau] {
        tau += 1;
    }
    if cmnd[tau] > YIN_VOICED {
        return None;
    }
    // Parabolic interpolation around the dip.
    let (a, b, c) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() > 1e-12 { (0.5 * (a - c) / denom).clamp(-1.0, 1.0) } else { 0.0 };
    Some(rate / (tau as f64 + shift))
}

/// An in-place radix-2 FFT.
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
        let angle = -2.0 * PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (angle * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f64, seconds: f64) -> Vec<f32> {
        (0..(seconds * f64::from(RATE)) as usize)
            .map(|n| {
                let t = n as f64 / f64::from(RATE);
                // A few harmonics, so it has a pitch YIN can find and an envelope.
                (1..=6).map(|k| (2.0 * PI * hz * f64::from(k) * t).sin() / f64::from(k)).sum::<f64>() as f32 * 0.2
            })
            .collect()
    }

    #[test]
    fn yin_finds_the_pitch() {
        let x = tone(180.0, 0.5);
        let f = analyse(&x);
        let mid = &f.frames[f.frames.len() / 2];
        let f0 = mid.f0.unwrap_or(0.0);
        assert!((f0 - 180.0).abs() < 1.5, "{f0}");
    }

    #[test]
    fn a_signal_against_itself_is_zero_everywhere() {
        let x = tone(150.0, 1.0);
        let f = analyse(&x);
        let p = compare(&f, &f).unwrap_or_else(|e| panic!("{e}"));
        let m = &p.measures;
        assert!(m.mcd_db < 1e-9 && m.spectral_db < 1e-9 && m.f0_rmse_cents < 1e-9, "{m:?}");
        assert!(m.timing_rms_ms < 1e-9 && (m.duration_ratio - 1.0).abs() < 1e-12, "{m:?}");
    }

    #[test]
    fn a_pitch_shift_shows_as_a_bias_in_cents() {
        let a = analyse(&tone(200.0, 1.0));
        let b = analyse(&tone(100.0 * 2f64.powf(11.0 / 12.0) * 1.0, 1.0));
        let p = compare(&a, &b).unwrap_or_else(|e| panic!("{e}"));
        // 200 Hz over 188.8 Hz is a semitone: 100 cents.
        assert!((p.measures.f0_bias_cents - 100.0).abs() < 5.0, "{:?}", p.measures);
        assert!(p.measures.f0_shape_cents < 5.0, "{:?}", p.measures);
    }

    #[test]
    fn silence_is_refused() {
        let quiet = analyse(&vec![0.0; 16_000]);
        let voice = analyse(&tone(150.0, 1.0));
        assert_eq!(compare(&quiet, &voice).err(), Some(ParityError::Silent("our audio")));
    }

    #[test]
    fn the_fft_matches_a_direct_dft() {
        let mut re: Vec<f64> = (0..16).map(|n: i32| f64::from((n * 7) % 5) - 2.0).collect();
        let input = re.clone();
        let mut im = vec![0.0; 16];
        fft(&mut re, &mut im);
        for k in 0..16 {
            let (mut r, mut i) = (0.0, 0.0);
            for (n, x) in input.iter().enumerate() {
                let a = -2.0 * PI * (k * n) as f64 / 16.0;
                r += x * a.cos();
                i += x * a.sin();
            }
            assert!((re[k] - r).abs() < 1e-9 && (im[k] - i).abs() < 1e-9);
        }
    }
}
