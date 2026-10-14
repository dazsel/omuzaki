//! Deterministic test signals for the VAD: silence, noise, tones and clicks.
//!
//! Everything is generated from a seed with a tiny xorshift generator, so a test or benchmark run is bit-identical on
//! every machine (the project's determinism rule). None of this is TTS or recorded speech.

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

/// A steady sine tone: a tonal non-speech sound (a hum at 50 Hz, a whistle at 1 kHz).
#[must_use]
pub fn tone(seconds: f32, hz: f32, amplitude: f32) -> Vec<f32> {
    (0..samples(seconds)).map(|i| amplitude * (TAU * hz * i as f32 / SAMPLE_RATE as f32).sin()).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_identical_signals() {
        assert_eq!(white_noise(0.1, 0.5, 7), white_noise(0.1, 0.5, 7));
        assert_ne!(white_noise(0.1, 0.5, 7), white_noise(0.1, 0.5, 8));
    }

    #[test]
    fn noise_stays_inside_its_amplitude_and_is_roughly_zero_mean() {
        let noise = white_noise(1.0, 0.25, 3);
        assert!(noise.iter().all(|s| s.abs() <= 0.25));
        assert!((noise.iter().sum::<f32>() / noise.len() as f32).abs() < 0.01);
    }

    #[test]
    fn tone_has_the_requested_length_and_peak() {
        let t = tone(0.5, 220.0, 0.4);
        assert_eq!(t.len(), samples(0.5));
        assert!((t.iter().fold(0.0f32, |m, s| m.max(s.abs())) - 0.4).abs() < 0.01);
    }
}
