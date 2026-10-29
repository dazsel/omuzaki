//! [`VoiceAnalyzer`]: microphone samples in, a [`VoiceFrame`] every [`HOP_S`] out.
//!
//! Per hop, over the last [`WINDOW_S`] of audio brought to about [`TARGET_RATE`]:
//!
//! 1. **Level.** RMS in dBFS, against a noise floor that follows the room: it drops at once to a quieter frame and
//!    creeps up at [`FLOOR_RISE_DB_S`] while nothing voiced is heard, so a fan that starts is learnt in seconds.
//! 2. **Voicing.** The normalised autocorrelation peak over pitch lags 70 to 400 Hz: vowels are periodic, fans,
//!    breath and keyboard clicks are not. A frame is *voiced* when it is periodic and [`VOICED_ABOVE_DB`] over the
//!    floor; *speaking* holds for [`HANGOVER_S`] after the last voiced frame, so a consonant does not end a sentence.
//! 3. **Formants.** Linear prediction of order [`LPC_ORDER`] (autocorrelation method, Levinson-Durbin) gives the vocal
//!    tract's spectral envelope; its first peaks in the F1 and F2 ranges are the formants, and the [`VowelProfile`]
//!    turns them into viseme weights, scaled by how open the voice is (its loudness).
//! 4. **Closures.** Against the sentence's loudest voiced level (falling [`PEAK_FALL_DB_S`] while nothing louder is
//!    heard): a hop that is not voiced and [`GAP_BELOW_DB`] under it while speaking is a stop's closure, and a voiced
//!    hop [`MURMUR_BELOW_DB`] under it with a first formant below [`MURMUR_F1_HZ`] is a nasal's murmur. Either is a
//!    lip closure for the mouth (`p b m`, and the tongue's `t d n k g`, which a drawn mouth may close on too).
//! 5. **Onsets.** The [`OnsetDetector`]'s syllable onsets (three-band onset strength, adaptive threshold, 120 ms
//!    refractory) on the analysis signal, each reported on the first voiced frame within [`ONSET_VOICED_WITHIN_S`] of
//!    it, so a keyboard click or a cough is not a syllable (brow kicks and nods in the life layer).
//!
//! Everything is allocated in [`VoiceAnalyzer::new`]; [`VoiceAnalyzer::push`] allocates nothing (`tests/no_alloc.rs`).
//! Deterministic: the same samples give the same frames.

use std::f32::consts::PI;

use crate::onset::OnsetDetector;
use crate::viseme::VowelProfile;

/// The rate the analysis runs at: input is decimated by a whole factor to near it. Formants of interest are below
/// 4 kHz, so 16 kHz keeps them with room.
pub const TARGET_RATE: f32 = 16_000.0;
/// Time between frames, seconds.
pub const HOP_S: f32 = 0.010;
/// The analysis window, seconds: two to three pitch periods of a low voice.
pub const WINDOW_S: f32 = 0.032;
/// Linear-prediction order: two poles per formant up to 4 kHz at 16 kHz, plus spares for the glottal tilt.
pub const LPC_ORDER: usize = 14;
/// The lowest and highest input rates accepted.
pub const RATE_RANGE: (u32, u32) = (8_000, 192_000);
/// A frame this far over the noise floor (and periodic) is voiced, dB.
pub const VOICED_ABOVE_DB: f32 = 9.0;
/// Nothing quieter than this is voice, dBFS, however quiet the room.
pub const SILENCE_DBFS: f32 = -60.0;
/// The autocorrelation peak a voiced frame reaches, `0..1`.
pub const PERIODICITY: f32 = 0.45;
/// How long *speaking* holds after the last voiced frame, seconds.
pub const HANGOVER_S: f32 = 0.25;
/// How fast the noise floor rises while nothing voiced is heard, dB per second.
pub const FLOOR_RISE_DB_S: f32 = 3.0;
/// An onset counts when a voiced frame follows it within this, seconds: voicing needs a few hops of a vowel to show.
pub const ONSET_VOICED_WITHIN_S: f32 = 0.06;
/// A hop this far under the sentence's peak, not voiced, while speaking, is a stop's closure, dB.
pub const GAP_BELOW_DB: f32 = 25.0;
/// A voiced hop this far under the sentence's peak, with a first formant below [`MURMUR_F1_HZ`], is a nasal, dB.
pub const MURMUR_BELOW_DB: f32 = 10.0;
/// See [`MURMUR_BELOW_DB`], Hz.
pub const MURMUR_F1_HZ: f32 = 400.0;
/// How fast the sentence's peak falls while nothing louder is voiced, dB per second.
pub const PEAK_FALL_DB_S: f32 = 4.0;
/// Loudness is 0 at this many dB over the floor and 1 at this plus [`LOUDNESS_SPAN_DB`].
const LOUDNESS_FROM_DB: f32 = 6.0;
/// See [`LOUDNESS_FROM_DB`].
const LOUDNESS_SPAN_DB: f32 = 30.0;
/// The envelope is evaluated every this many Hz between [`ENV_LO_HZ`] and [`ENV_HI_HZ`].
const ENV_STEP_HZ: f32 = 20.0;
const ENV_LO_HZ: f32 = 150.0;
const ENV_HI_HZ: f32 = 3500.0;
/// F1 is searched in this range, Hz.
const F1_RANGE: (f32, f32) = (200.0, 1100.0);
/// F2 is searched in this range, Hz, and at least [`F2_GAP_HZ`] above F1.
const F2_RANGE: (f32, f32) = (600.0, 3200.0);
const F2_GAP_HZ: f32 = 200.0;
/// Pitch range of the voicing check, Hz.
const PITCH_RANGE: (f32, f32) = (70.0, 400.0);

/// What the analyser heard in one hop. `Copy`, fixed-size.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct VoiceFrame {
    /// Frames since the analyser started; gaps never happen (each hop makes one).
    pub seq: u64,
    /// A voice is speaking: voiced now or within [`HANGOVER_S`]. What the life layer's speech nods and the captions'
    /// "listening" read.
    pub speaking: bool,
    /// This hop is a vowel or other voiced sound.
    pub voiced: bool,
    /// How loud the voice is over the room, `0..1`.
    pub loudness: f32,
    /// The five viseme weights (`aa`, `ih`, `ou`, `ee`, `oh`), each `0..1`, summing to at most 1: zero when not voiced.
    pub visemes: [f32; 5],
    /// Onsets (stressed syllables) since the analyser started. A count, not a flag, so a reader that skips frames
    /// still sees every one.
    pub onsets: u32,
    /// The F1 and F2 found, Hz; zero when not voiced. For the vowel calibration and the meters.
    pub formants: [f32; 2],
    /// The lips are shut for a closure: a stop's gap or a nasal's murmur, `0` or `1`.
    pub closure: f32,
}

/// Why an analyser could not be made.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VoiceError {
    /// The sample rate is outside [`RATE_RANGE`].
    #[error("sample rate {0} Hz is outside RATE_RANGE ({lo} to {hi} Hz)", lo = RATE_RANGE.0, hi = RATE_RANGE.1)]
    Rate(u32),
}

/// The analyser. One per voice; feed it with [`push`](Self::push) from one thread.
#[derive(Clone, Debug)]
pub struct VoiceAnalyzer {
    profile: VowelProfile,
    /// Input samples averaged into one analysis sample.
    factor: usize,
    /// The analysis rate, Hz.
    rate: f32,
    acc: f32,
    acc_len: usize,
    /// The newest `window` analysis samples, circular, `head` the oldest.
    ring: Box<[f32]>,
    head: usize,
    filled: usize,
    hop: usize,
    since_hop: usize,
    /// The Hamming window, and scratch the window is copied into.
    taper: Box<[f32]>,
    work: Box<[f32]>,
    /// `cos` and `sin` of `ω_k · i` for each envelope bin `k` and lag `i ≤ LPC_ORDER`, bin-major.
    cos: Box<[f32]>,
    sin: Box<[f32]>,
    envelope: Box<[f32]>,
    floor_db: f32,
    /// The sentence's loudest voiced level, dBFS, falling slowly.
    peak_db: f32,
    hangover: u32,
    hangover_hops: u32,
    onset: OnsetDetector,
    /// Hops left for a voiced frame to take the newest onset; 0 when none waits.
    onset_waits: u32,
    onset_hops: u32,
    floor_rise: f32,
    frame: VoiceFrame,
}

impl VoiceAnalyzer {
    /// An analyser for mono samples at `sample_rate` Hz, classifying vowels against `profile`. Cold: allocates.
    ///
    /// # Errors
    /// [`VoiceError::Rate`] for a rate outside [`RATE_RANGE`].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small counts
    pub fn new(sample_rate: u32, profile: VowelProfile) -> Result<Self, VoiceError> {
        if !(RATE_RANGE.0..=RATE_RANGE.1).contains(&sample_rate) {
            return Err(VoiceError::Rate(sample_rate));
        }
        let factor = ((sample_rate as f32 / TARGET_RATE).round() as usize).max(1);
        let rate = sample_rate as f32 / factor as f32;
        let window = (WINDOW_S * rate).round() as usize;
        let hop = (HOP_S * rate).round() as usize;
        let taper: Box<[f32]> =
            (0..window).map(|n| 0.54 - 0.46 * (2.0 * PI * n as f32 / (window - 1) as f32).cos()).collect();
        let bins = ((ENV_HI_HZ - ENV_LO_HZ) / ENV_STEP_HZ) as usize + 1;
        let mut cos = vec![0.0; bins * (LPC_ORDER + 1)];
        let mut sin = vec![0.0; bins * (LPC_ORDER + 1)];
        for k in 0..bins {
            let w = 2.0 * PI * (ENV_LO_HZ + k as f32 * ENV_STEP_HZ) / rate;
            for i in 0..=LPC_ORDER {
                cos[k * (LPC_ORDER + 1) + i] = (w * i as f32).cos();
                sin[k * (LPC_ORDER + 1) + i] = (w * i as f32).sin();
            }
        }
        let hops_per_s = 1.0 / HOP_S;
        Ok(Self {
            profile,
            factor,
            rate,
            acc: 0.0,
            acc_len: 0,
            ring: vec![0.0; window].into(),
            head: 0,
            filled: 0,
            hop,
            since_hop: 0,
            taper,
            work: vec![0.0; window].into(),
            cos: cos.into(),
            sin: sin.into(),
            envelope: vec![0.0; bins].into(),
            floor_db: SILENCE_DBFS - 10.0,
            peak_db: SILENCE_DBFS,
            hangover: 0,
            hangover_hops: (HANGOVER_S * hops_per_s).round() as u32,
            onset: OnsetDetector::new(rate.round() as u32),
            onset_waits: 0,
            onset_hops: (ONSET_VOICED_WITHIN_S * hops_per_s).round() as u32,
            floor_rise: FLOOR_RISE_DB_S * HOP_S,
            frame: VoiceFrame::default(),
        })
    }

    /// Replaces the vowel profile (a finished vowel calibration). Takes effect from the next frame.
    pub fn set_profile(&mut self, profile: VowelProfile) {
        self.profile = profile;
    }

    /// The newest frame (all zero before the first hop).
    pub fn frame(&self) -> &VoiceFrame {
        &self.frame
    }

    /// Feeds `samples` (mono, `-1..1`) and calls `on_frame` for every hop they complete. Allocates nothing.
    #[allow(clippy::cast_precision_loss)] // factor is small
    pub fn push(&mut self, samples: &[f32], mut on_frame: impl FnMut(&VoiceFrame)) {
        for &s in samples {
            self.acc += if s.is_finite() { s } else { 0.0 };
            self.acc_len += 1;
            if self.acc_len < self.factor {
                continue;
            }
            let sample = self.acc / self.factor as f32;
            (self.acc, self.acc_len) = (0.0, 0);
            self.ring[self.head] = sample;
            self.head = (self.head + 1) % self.ring.len();
            self.filled = (self.filled + 1).min(self.ring.len());
            if self.onset.step(sample).is_some() {
                self.onset_waits = self.onset_hops + 1;
            }
            self.since_hop += 1;
            if self.since_hop >= self.hop && self.filled == self.ring.len() {
                self.since_hop = 0;
                self.analyze();
                on_frame(&self.frame);
            }
        }
    }

    #[allow(clippy::cast_precision_loss)] // window lengths are small
    fn analyze(&mut self) {
        let n = self.ring.len();
        // The window in time order.
        for (i, w) in self.work.iter_mut().enumerate() {
            *w = self.ring[(self.head + i) % n];
        }
        let energy = self.work.iter().map(|x| x * x).sum::<f32>() / n as f32;
        let db = 10.0 * (energy + 1e-12).log10();
        let periodic = periodicity(&self.work, self.rate) >= PERIODICITY;
        let voiced = periodic && db > SILENCE_DBFS && db > self.floor_db + VOICED_ABOVE_DB;

        // The floor drops at once to anything quieter and creeps up only while nothing voiced is heard.
        if db < self.floor_db {
            self.floor_db = db;
        } else if !voiced {
            self.floor_db += self.floor_rise.min(db - self.floor_db);
        }
        let loudness = ((db - self.floor_db - LOUDNESS_FROM_DB) / LOUDNESS_SPAN_DB).clamp(0.0, 1.0);

        let onset = voiced && self.onset_waits > 0;
        self.onset_waits = if onset { 0 } else { self.onset_waits.saturating_sub(1) };

        self.hangover = if voiced { self.hangover_hops } else { self.hangover.saturating_sub(1) };
        let (visemes, formants) = if voiced { self.vowel(loudness) } else { ([0.0; 5], [0.0; 2]) };
        self.peak_db = if voiced { self.peak_db.max(db) } else { self.peak_db } - PEAK_FALL_DB_S * HOP_S;
        let speaking = voiced || self.hangover > 0;
        let gap = speaking && !voiced && db < self.peak_db - GAP_BELOW_DB;
        let murmur = voiced && formants[0] < MURMUR_F1_HZ && db < self.peak_db - MURMUR_BELOW_DB;
        self.frame = VoiceFrame {
            seq: self.frame.seq + 1,
            speaking,
            voiced,
            loudness: if voiced || self.hangover > 0 { loudness } else { 0.0 },
            visemes,
            onsets: self.frame.onsets.wrapping_add(u32::from(onset)),
            formants,
            closure: if gap || murmur { 1.0 } else { 0.0 },
        };
    }

    /// The viseme weights and formants of the window in `work` (raw samples), scaled by `loudness`.
    fn vowel(&mut self, loudness: f32) -> ([f32; 5], [f32; 2]) {
        // Pre-emphasis flattens the glottal tilt so the high formants are not lost; then the taper.
        let mut prev = 0.0;
        for (x, t) in self.work.iter_mut().zip(self.taper.iter()) {
            let raw = *x;
            *x = (raw - 0.97 * prev) * t;
            prev = raw;
        }
        let mut r = [0.0f32; LPC_ORDER + 1];
        for (lag, r) in r.iter_mut().enumerate() {
            *r = self.work[lag..].iter().zip(self.work.iter()).map(|(a, b)| a * b).sum();
        }
        let Some(a) = levinson(&r) else { return ([0.0; 5], [0.0; 2]) };
        let stride = LPC_ORDER + 1;
        for (k, e) in self.envelope.iter_mut().enumerate() {
            let (cos, sin) = (&self.cos[k * stride..(k + 1) * stride], &self.sin[k * stride..(k + 1) * stride]);
            let (mut re, mut im) = (0.0, 0.0);
            for ((a, c), s) in a.iter().zip(cos).zip(sin) {
                re += a * c;
                im -= a * s;
            }
            *e = -10.0 * (re * re + im * im + 1e-12).log10();
        }
        let Some(f1) = self.peak(F1_RANGE.0, F1_RANGE.1) else { return ([0.0; 5], [0.0; 2]) };
        // Back vowels can merge F1 and F2 into one peak; then F2 sits just above F1, where the envelope is still high.
        let f2 = self.peak((f1 + F2_GAP_HZ).max(F2_RANGE.0), F2_RANGE.1).unwrap_or(f1 + F2_GAP_HZ);
        let open = loudness.powf(0.6);
        let weights = self.profile.classify(f1, f2).map(|w| w * open);
        (weights, [f1, f2])
    }

    /// The frequency of the first local maximum of the envelope in `lo..hi` Hz, refined by a parabola through it and
    /// its neighbours.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // bin indices
    fn peak(&self, lo: f32, hi: f32) -> Option<f32> {
        let bin = |hz: f32| ((hz - ENV_LO_HZ) / ENV_STEP_HZ).round().max(1.0) as usize;
        let e = &self.envelope;
        let (from, to) = (bin(lo), bin(hi).min(e.len() - 2));
        (from..=to).find(|&k| e[k] > e[k - 1] && e[k] >= e[k + 1]).map(|k| {
            let (l, c, r) = (e[k - 1], e[k], e[k + 1]);
            let denom = l - 2.0 * c + r;
            let offset = if denom.abs() > 1e-9 { (0.5 * (l - r) / denom).clamp(-0.5, 0.5) } else { 0.0 };
            ENV_LO_HZ + (k as f32 + offset) * ENV_STEP_HZ
        })
    }
}

/// The normalised autocorrelation peak of `x` over pitch lags, `0..1`, on every other sample (pitch needs no more).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // lag bounds are small and positive
fn periodicity(x: &[f32], rate: f32) -> f32 {
    let lo = (rate / PITCH_RANGE.1 / 2.0) as usize;
    let hi = ((rate / PITCH_RANGE.0 / 2.0) as usize).min(x.len() / 4);
    let half = |i: usize| x[2 * i];
    let len = x.len() / 2;
    let r0: f32 = (0..len).map(|i| half(i) * half(i)).sum();
    if r0 <= 1e-12 || lo >= hi {
        return 0.0;
    }
    let mut best = 0.0f32;
    for lag in lo.max(1)..=hi {
        let (mut r, mut e) = (0.0, 0.0);
        for i in 0..len - lag {
            r += half(i) * half(i + lag);
            e += half(i + lag) * half(i + lag);
        }
        // Normalised by both segments' energy, so a decaying window does not read as aperiodic.
        let head: f32 = (0..len - lag).map(|i| half(i) * half(i)).sum();
        let norm = (head * e).sqrt();
        if norm > 1e-12 {
            best = best.max(r / norm);
        }
    }
    best
}

/// Linear-prediction coefficients `a` (with `a[0] = 1`) from autocorrelation `r`, by Levinson-Durbin; `None` for a
/// silent or unstable window.
fn levinson(r: &[f32; LPC_ORDER + 1]) -> Option<[f32; LPC_ORDER + 1]> {
    if r[0] <= 1e-12 {
        return None;
    }
    let mut a = [0.0f32; LPC_ORDER + 1];
    let mut prev = [0.0f32; LPC_ORDER + 1];
    a[0] = 1.0;
    // A whisker of white noise on r[0] keeps the recursion stable on near-singular windows.
    let mut error = r[0] * 1.0001;
    for i in 1..=LPC_ORDER {
        let acc: f32 = (1..i).map(|j| a[j] * r[i - j]).sum();
        let k = -(r[i] + acc) / error;
        if !k.is_finite() || k.abs() >= 1.0 {
            return None;
        }
        prev[..i].copy_from_slice(&a[..i]);
        for j in 1..i {
            a[j] = prev[j] + k * prev[i - j];
        }
        a[i] = k;
        error *= 1.0 - k * k;
    }
    Some(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{self, Vowel};
    use crate::viseme::Viseme;

    fn frames(rate: u32, samples: &[f32]) -> Vec<VoiceFrame> {
        let mut analyzer = VoiceAnalyzer::new(rate, VowelProfile::default()).unwrap();
        let mut out = Vec::new();
        analyzer.push(samples, |f| out.push(*f));
        out
    }

    #[test]
    fn rates_outside_the_range_are_refused() {
        assert_eq!(VoiceAnalyzer::new(4_000, VowelProfile::default()).err(), Some(VoiceError::Rate(4_000)));
        assert!(VoiceAnalyzer::new(48_000, VowelProfile::default()).is_ok());
    }

    #[test]
    fn a_hop_every_ten_milliseconds_at_any_rate() {
        for rate in [16_000, 44_100, 48_000] {
            let out = frames(rate, &synth::noise(rate, 1.0, 0.001));
            // The first frame waits for a full window.
            assert!((96..=98).contains(&out.len()), "{rate}: {}", out.len());
            assert!(out.windows(2).all(|w| w[1].seq == w[0].seq + 1));
        }
    }

    #[test]
    fn silence_and_noise_are_not_voice() {
        for samples in [vec![0.0; 16_000], synth::noise(16_000, 2.0, 0.05)] {
            let out = frames(16_000, &samples);
            assert!(
                out.iter().all(|f| !f.voiced && f.visemes.iter().all(|&w| w == 0.0)),
                "{:?}",
                out.iter().find(|f| f.voiced)
            );
        }
    }

    #[test]
    fn a_vowel_is_voiced_loud_and_its_own_viseme() {
        for (rate, f0, tract) in [(16_000, 120.0, 1.0), (48_000, 220.0, 1.0), (44_100, 160.0, 0.95)] {
            for viseme in Viseme::ALL {
                let mut samples = synth::noise(rate, 0.5, 0.002);
                samples.extend(synth::vowel(Vowel::of(viseme, f0, tract, 0.4), rate, 0.6, 0.002));
                let out = frames(rate, &samples);
                let speech = &out[out.len() - 30..];
                assert!(speech.iter().all(|f| f.voiced && f.speaking), "{viseme:?} at {rate}");
                assert!(speech.iter().all(|f| f.loudness > 0.5), "{viseme:?}: {:?}", speech[0].loudness);
                let hits = speech
                    .iter()
                    .filter(|f| (0..5).max_by(|&a, &b| f.visemes[a].total_cmp(&f.visemes[b])) == Some(viseme.index()))
                    .count();
                assert!(hits >= 27, "{viseme:?} at {rate} Hz, f0 {f0}: {hits}/30, formants {:?}", speech[0].formants);
                assert_eq!(out.iter().filter(|f| f.onsets > 0).map(|f| f.onsets).min(), Some(1), "one onset");
            }
        }
    }

    #[test]
    fn speaking_holds_through_a_short_gap_then_ends() {
        let rate = 16_000;
        let mut samples = synth::vowel(Vowel::of(Viseme::Aa, 130.0, 1.0, 0.4), rate, 0.5, 0.0);
        samples.extend(vec![0.0; 8_000]);
        let out = frames(rate, &samples);
        let after: Vec<_> = out.iter().skip_while(|f| f.voiced || !f.speaking && f.seq < 10).collect();
        assert!(after[0].speaking && !after[0].voiced, "the hangover holds");
        assert!(!out.last().unwrap().speaking, "and ends");
        assert_eq!(out.last().unwrap().loudness, 0.0);
    }

    #[test]
    fn the_same_samples_give_the_same_frames() {
        let samples = synth::vowel(Vowel::of(Viseme::Ee, 180.0, 1.0, 0.3), 48_000, 0.3, 0.01);
        assert_eq!(frames(48_000, &samples), frames(48_000, &samples));
    }
}
