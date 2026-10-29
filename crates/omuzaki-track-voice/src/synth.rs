//! A deterministic formant synthesiser: vowels, a few consonants and silence made from numbers, for tests, benches and evidence, so no
//! recording of a real person is ever needed (`CLAUDE.md`, "The repository is public").
//!
//! The classic source-filter model: a glottal pulse train (a Rosenberg pulse at the pitch `f0`, with a little
//! deterministic jitter) through a cascade of two-pole resonators at the vowel's formants, plus optional white noise
//! from a fixed-seed generator.

use std::f32::consts::PI;

use crate::phoneme::{Phone, PhoneSpan};
use crate::viseme::Viseme;

/// One vowel to synthesise.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Vowel {
    /// Pitch, Hz.
    pub f0: f32,
    /// The first three formants, Hz.
    pub formants: [f32; 3],
    /// Peak amplitude of the output, `0..1` of full scale.
    pub level: f32,
}

impl Vowel {
    /// A typical adult voice saying `viseme`'s vowel at pitch `f0`, at `level`. The formants are the textbook means of
    /// Japanese vowels (between the usual adult male and female values), scaled by `tract` (1.0 average; above 1 a
    /// shorter vocal tract, higher formants).
    pub fn of(viseme: Viseme, f0: f32, tract: f32, level: f32) -> Self {
        let [f1, f2] = viseme.formants();
        Self { f0, formants: [f1 * tract, f2 * tract, 2900.0 * tract], level }
    }
}

/// Bandwidths of the three resonators, Hz.
const BANDWIDTHS: [f32; 3] = [80.0, 100.0, 140.0];

/// A two-pole resonator (Klatt's), unity gain at DC.
#[derive(Copy, Clone, Debug)]
struct Resonator {
    a: f32,
    b: f32,
    c: f32,
    y1: f32,
    y2: f32,
}

impl Resonator {
    fn new(freq: f32, bandwidth: f32, rate: f32) -> Self {
        let r = (-PI * bandwidth / rate).exp();
        let c = -r * r;
        let b = 2.0 * r * (2.0 * PI * freq / rate).cos();
        Self { a: 1.0 - b - c, b, c, y1: 0.0, y2: 0.0 }
    }

    fn step(&mut self, x: f32) -> f32 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// A fixed-seed generator for the jitter and the noise (xorshift; the same seed gives the same samples everywhere).
#[derive(Copy, Clone, Debug)]
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        #[allow(clippy::cast_precision_loss)] // 24 bits of a u32 are exact in f32
        let unit = (self.0 >> 8) as f32 / (1u32 << 24) as f32;
        unit * 2.0 - 1.0
    }
}

/// `seconds` of `vowel` at `rate` Hz, with white noise at `noise` of full scale.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // sample counts
pub fn vowel(vowel: Vowel, rate: u32, seconds: f32, noise: f32) -> Vec<f32> {
    let fs = rate as f32;
    let len = (seconds * fs) as usize;
    let mut out = Vec::with_capacity(len);
    let mut resonators = [0, 1, 2].map(|i| Resonator::new(vowel.formants[i], BANDWIDTHS[i], fs));
    let mut rng = Noise(0x9E37_79B9);
    let mut phase = 0.0f32;
    let mut period = fs / vowel.f0;
    for _ in 0..len {
        // Rosenberg pulse: opening for 40% of the period, closing for 16%, closed for the rest.
        let t = phase / period;
        let pulse = if t < 0.4 {
            0.5 * (1.0 - (PI * t / 0.4).cos())
        } else if t < 0.56 {
            ((PI / 2.0) * (t - 0.4) / 0.16).cos()
        } else {
            0.0
        };
        let mut y = pulse;
        for r in &mut resonators {
            y = r.step(y);
        }
        out.push(y);
        phase += 1.0;
        if phase >= period {
            phase -= period;
            period = fs / (vowel.f0 * (1.0 + 0.01 * rng.next()));
        }
    }
    // The lips radiate the derivative of the flow; a first difference stands in for it.
    let mut prev = 0.0;
    for s in &mut out {
        let x = *s;
        *s = x - prev;
        prev = x;
    }
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-9);
    for s in &mut out {
        *s = *s / peak * vowel.level + noise * rng.next();
    }
    out
}

/// One sound of a synthetic line ([`utterance`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Sound {
    /// A vowel for this many seconds.
    Vowel(Viseme, f32),
    /// `m`: a voiced murmur through the nose (low first formant, about a third of a vowel's level), lips shut.
    Nasal(f32),
    /// `p` or `b` (`lips`), else `t` or `k`: a silent closure ending in a short burst.
    Stop {
        /// The lips make the closure.
        lips: bool,
        /// Seconds, burst included.
        seconds: f32,
    },
    /// `s`: a hiss.
    Fricative(f32),
    /// A pause.
    Pause(f32),
}

impl Sound {
    /// Its length, seconds.
    pub fn seconds(self) -> f32 {
        match self {
            Self::Vowel(_, s)
            | Self::Nasal(s)
            | Self::Fricative(s)
            | Self::Pause(s)
            | Self::Stop { seconds: s, .. } => s,
        }
    }

    /// The phone it is, as the mouth sees it.
    pub fn phone(self) -> Phone {
        match self {
            Self::Vowel(v, _) => Phone::Vowel(v),
            Self::Nasal(_) | Self::Stop { lips: true, .. } => Phone::Bilabial,
            Self::Stop { lips: false, .. } | Self::Fricative(_) => Phone::Consonant,
            Self::Pause(_) => Phone::Silence,
        }
    }
}

/// The burst at the end of a stop, seconds.
const BURST_S: f32 = 0.015;
/// The room under a synthetic line, of full scale.
const ROOM: f32 = 0.002;

/// A synthetic line: `sounds` one after another at pitch `f0` and peak `level`, over a quiet room, with the exact span
/// of each (the ground truth a lip-sync evaluation scores against, `crate::eval`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // sample counts
pub fn utterance(sounds: &[Sound], f0: f32, rate: u32, level: f32) -> (Vec<f32>, Vec<PhoneSpan>) {
    let fs = rate as f32;
    let mut pcm: Vec<f32> = Vec::new();
    let mut spans = Vec::with_capacity(sounds.len());
    for &sound in sounds {
        let start = pcm.len();
        let len = (sound.seconds() * fs) as usize;
        match sound {
            Sound::Vowel(viseme, seconds) => pcm.extend(vowel(Vowel::of(viseme, f0, 1.0, level), rate, seconds, 0.0)),
            Sound::Nasal(seconds) => {
                let murmur = Vowel { f0, formants: [270.0, 1100.0, 2300.0], level: level * 0.3 };
                pcm.extend(vowel(murmur, rate, seconds, 0.0));
            }
            Sound::Stop { seconds, .. } => {
                let burst = ((BURST_S.min(seconds)) * fs) as usize;
                pcm.extend(std::iter::repeat_n(0.0, len.saturating_sub(burst)));
                pcm.extend(noise(rate, burst as f32 / fs, level * 0.15));
            }
            Sound::Fricative(seconds) => pcm.extend(noise(rate, seconds, level * 0.12)),
            Sound::Pause(seconds) => pcm.extend(std::iter::repeat_n(0.0, (seconds * fs) as usize)),
        }
        pcm.resize(start + len, 0.0);
        let at = |i: usize| f64::from(i as f32 / fs);
        spans.push(PhoneSpan { phone: sound.phone(), start_s: at(start), end_s: at(start + len) });
    }
    let room = noise(rate, pcm.len() as f32 / fs + 1.0, ROOM);
    for (s, n) in pcm.iter_mut().zip(room) {
        *s += n;
    }
    (pcm, spans)
}

/// `seconds` of white noise at `level` of full scale: a quiet room, or a fan.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // sample counts
pub fn noise(rate: u32, seconds: f32, level: f32) -> Vec<f32> {
    let mut rng = Noise(0x1234_5678);
    (0..(seconds * rate as f32) as usize).map(|_| level * rng.next()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_call_gives_the_same_samples() {
        let v = Vowel::of(Viseme::Aa, 140.0, 1.0, 0.5);
        assert_eq!(vowel(v, 16_000, 0.1, 0.01), vowel(v, 16_000, 0.1, 0.01));
        let samples = vowel(v, 16_000, 0.1, 0.0);
        assert_eq!(samples.len(), 1600);
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 1e-4, "{peak}");
    }
}
