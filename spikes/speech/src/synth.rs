//! Deterministic test signals for the VAD: noise, hum, clicks, a speech-like vowel train, and SNR mixing.
//!
//! Everything is generated from a seed with a tiny xorshift generator, so a test or benchmark run is bit-identical
//! on every machine (the project's determinism rule). None of this is TTS or recorded speech.

use std::f32::consts::TAU;

use crate::SAMPLE_RATE;

/// A xorshift64* generator: small, fast and good enough for noise.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed` (a zero seed is remapped, since xorshift cannot leave the all-zero state).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    /// The next value, uniform in -1.0..1.0.
    pub fn next_signed(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as u32; // top 24 bits
        bits as f32 / (1u32 << 23) as f32 - 1.0
    }

    /// The next value, uniform in `lo..hi`.
    pub fn next_range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next_signed() * 0.5 + 0.5) * (hi - lo)
    }
}

/// Samples in `seconds` of audio.
#[must_use]
pub fn samples(seconds: f32) -> usize {
    (seconds * SAMPLE_RATE as f32) as usize
}

/// Digital silence.
#[must_use]
pub fn silence(seconds: f32) -> Vec<f32> {
    vec![0.0; samples(seconds)]
}

/// White noise with the given peak amplitude.
#[must_use]
pub fn white_noise(seconds: f32, amplitude: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    (0..samples(seconds)).map(|_| rng.next_signed() * amplitude).collect()
}

/// Pink-ish noise (Paul Kellet's three-pole filter on white noise) at roughly the given peak amplitude: closer to room
/// and fan noise than white noise is.
#[must_use]
pub fn pink_noise(seconds: f32, amplitude: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let (mut b0, mut b1, mut b2) = (0.0f32, 0.0f32, 0.0f32);
    (0..samples(seconds))
        .map(|_| {
            let white = rng.next_signed();
            b0 = 0.99765 * b0 + white * 0.099_046;
            b1 = 0.963 * b1 + white * 0.296_516_4;
            b2 = 0.57 * b2 + white * 1.052_691_3;
            (b0 + b1 + b2 + white * 0.1848) * 0.25 * amplitude
        })
        .collect()
}

/// Mains hum: a fundamental plus two harmonics, a steady tonal non-speech sound.
#[must_use]
pub fn hum(seconds: f32, fundamental_hz: f32, amplitude: f32) -> Vec<f32> {
    (0..samples(seconds))
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            let phase = TAU * fundamental_hz * t;
            amplitude * (phase.sin() * 0.6 + (2.0 * phase).sin() * 0.3 + (3.0 * phase).sin() * 0.1)
        })
        .collect()
}

/// Keyboard-like clicks: a 3 ms decaying noise burst every `period_s`.
#[must_use]
pub fn clicks(seconds: f32, period_s: f32, amplitude: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let period = samples(period_s).max(1);
    let burst = samples(0.003);
    (0..samples(seconds))
        .map(|i| {
            let at = i % period;
            if at < burst { rng.next_signed() * amplitude * (1.0 - at as f32 / burst as f32) } else { 0.0 }
        })
        .collect()
}

/// A two-pole resonator, the building block of a formant filter (Klatt 1980).
struct Resonator {
    a: f32,
    b: f32,
    c: f32,
    y1: f32,
    y2: f32,
}

impl Resonator {
    fn new(centre_hz: f32, bandwidth_hz: f32) -> Self {
        let r = (-std::f32::consts::PI * bandwidth_hz / SAMPLE_RATE as f32).exp();
        let c = -r * r;
        let b = 2.0 * r * (TAU * centre_hz / SAMPLE_RATE as f32).cos();
        Self { a: 1.0 - b - c, b, c, y1: 0.0, y2: 0.0 }
    }

    fn step(&mut self, x: f32) -> f32 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// First three formants (Hz) of five vowels.
const VOWELS: [[f32; 3]; 5] = [
    [730.0, 1090.0, 2440.0],
    [270.0, 2290.0, 3010.0],
    [300.0, 870.0, 2240.0],
    [530.0, 1840.0, 2480.0],
    [570.0, 840.0, 2410.0],
];

/// A train of voiced, vowel-like syllables with a gliding pitch: a glottal pulse train through three formant filters,
/// shaped by a syllable envelope with short gaps between. It has the spectral and temporal structure of speech but is
/// not speech, so whether the VAD calls it speech is an observation, not a pass/fail.
#[must_use]
pub fn voiced_syllables(syllables: usize, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for _ in 0..syllables {
        let vowel = VOWELS[(rng.next_range(0.0, 5.0) as usize).min(4)];
        let (start_hz, end_hz) = (rng.next_range(100.0, 150.0), rng.next_range(90.0, 140.0));
        let length = samples(rng.next_range(0.18, 0.28));
        let mut formants =
            [Resonator::new(vowel[0], 80.0), Resonator::new(vowel[1], 100.0), Resonator::new(vowel[2], 150.0)];
        let mut phase = 0.0f32;
        for i in 0..length {
            let progress = i as f32 / length as f32;
            phase += (start_hz + (end_hz - start_hz) * progress) / SAMPLE_RATE as f32;
            let pulse = if phase >= 1.0 {
                phase -= 1.0;
                1.0
            } else {
                0.0
            };
            let voiced: f32 = formants.iter_mut().zip([1.0, 0.6, 0.3]).map(|(f, gain)| f.step(pulse) * gain).sum();
            // Raised-cosine envelope: no clicks at the syllable edges.
            let envelope = 0.5 - 0.5 * (TAU * progress).cos();
            out.push(voiced * envelope);
        }
        out.extend(silence(rng.next_range(0.06, 0.12)));
    }
    normalise_peak(&mut out, 0.5);
    out
}

/// Scales `signal` so its largest sample magnitude is `peak`.
pub fn normalise_peak(signal: &mut [f32], peak: f32) {
    let max = signal.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if max > 0.0 {
        for sample in signal.iter_mut() {
            *sample *= peak / max;
        }
    }
}

/// Root-mean-square level of `signal` (0.0 when empty).
#[must_use]
pub fn rms(signal: &[f32]) -> f32 {
    if signal.is_empty() {
        return 0.0;
    }
    (signal.iter().map(|s| s * s).sum::<f32>() / signal.len() as f32).sqrt()
}

/// Adds `noise` (repeated or cut to length) to `signal` so the speech-to-noise power ratio is `snr_db`.
#[must_use]
pub fn mix_at_snr(signal: &[f32], noise: &[f32], snr_db: f32) -> Vec<f32> {
    if noise.is_empty() {
        return signal.to_vec();
    }
    let gain = rms(signal) / (rms(noise).max(f32::MIN_POSITIVE) * 10f32.powf(snr_db / 20.0));
    signal.iter().zip(noise.iter().cycle()).map(|(s, n)| s + n * gain).collect()
}

/// Frame-wise RMS in dBFS for consecutive `frame`-sample windows (the last partial window is padded with silence).
#[must_use]
pub fn frame_levels_db(signal: &[f32], frame: usize) -> Vec<f32> {
    signal.chunks(frame).map(|c| 20.0 * (rms(c) * (c.len() as f32 / frame as f32).sqrt()).max(1e-9).log10()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_identical_signals() {
        assert_eq!(white_noise(0.1, 0.5, 7), white_noise(0.1, 0.5, 7));
        assert_ne!(white_noise(0.1, 0.5, 7), white_noise(0.1, 0.5, 8));
        assert_eq!(voiced_syllables(3, 1), voiced_syllables(3, 1));
    }

    #[test]
    fn noise_stays_inside_its_amplitude() {
        assert!(white_noise(0.5, 0.25, 3).iter().all(|s| s.abs() <= 0.25));
        assert!(pink_noise(0.5, 0.25, 3).iter().all(|s| s.abs() <= 0.25 * 1.5));
    }

    #[test]
    fn rng_is_roughly_zero_mean() {
        let mut rng = Rng::new(42);
        let mean = (0..100_000).map(|_| rng.next_signed()).sum::<f32>() / 100_000.0;
        assert!(mean.abs() < 0.01, "mean {mean}");
    }

    #[test]
    fn mix_hits_the_requested_snr() {
        let signal = hum(1.0, 200.0, 0.3);
        let noise = white_noise(1.0, 1.0, 5);
        for snr in [0.0f32, 10.0, 20.0] {
            let mixed = mix_at_snr(&signal, &noise, snr);
            let residual: Vec<f32> = mixed.iter().zip(&signal).map(|(m, s)| m - s).collect();
            let measured = 20.0 * (rms(&signal) / rms(&residual)).log10();
            assert!((measured - snr).abs() < 0.01, "wanted {snr} dB, got {measured}");
        }
    }

    #[test]
    fn syllable_train_is_voiced_and_bounded() {
        let s = voiced_syllables(4, 9);
        assert!(s.len() > samples(0.7));
        assert!((s.iter().fold(0.0f32, |m, x| m.max(x.abs())) - 0.5).abs() < 1e-5);
        // Gaps between syllables are exactly silent.
        assert!(s.iter().filter(|x| **x == 0.0).count() > samples(0.2));
    }

    #[test]
    fn frame_levels_of_silence_are_floor_and_of_full_scale_are_near_zero_db() {
        assert!(frame_levels_db(&silence(0.1), 512).iter().all(|&l| l < -150.0));
        let square = vec![1.0f32; 1024];
        assert!(frame_levels_db(&square, 512).iter().all(|&l| l.abs() < 1e-3));
    }
}
