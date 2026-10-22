//! The clips `audio.fx.render` and `audio.fx.compare` put through a preset: a synthetic voice made here, or a WAV
//! file, and the WAV the render returns. Offline only.
//!
//! The synthetic voice is a Rosenberg glottal pulse train (differentiated for the lips' radiation, with a little
//! seeded period jitter) through four Klatt formant resonators set to one vowel (Peterson and Barney's adult averages).
//! It is not speech, but it has what an A/B needs: a known F0 and known formants to move.

use std::f64::consts::PI;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::dsp::{FS, noise};

/// A vowel of the synthetic voice.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Vowel {
    /// As in "father".
    #[default]
    A,
    /// As in "bed".
    E,
    /// As in "beet".
    I,
    /// As in "law".
    O,
    /// As in "boot".
    U,
}

impl Vowel {
    /// F1 to F4 of the vowel, in Hz.
    #[must_use]
    pub const fn formants(self) -> [f64; 4] {
        match self {
            Self::A => [730.0, 1090.0, 2440.0, 3400.0],
            Self::E => [530.0, 1840.0, 2480.0, 3500.0],
            Self::I => [270.0, 2290.0, 3010.0, 3500.0],
            Self::O => [570.0, 840.0, 2410.0, 3400.0],
            Self::U => [300.0, 870.0, 2240.0, 3400.0],
        }
    }
}

/// The synthetic voice to render.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Synthetic {
    /// The fundamental, 60 to 500 Hz. Default 120.
    #[serde(default = "default_f0")]
    pub f0_hz: f64,
    /// The vowel. Default `a`.
    #[serde(default)]
    pub vowel: Vowel,
    /// How long, 0.5 to 30 s. Default 2.
    #[serde(default = "default_seconds")]
    pub seconds: f64,
}

const fn default_f0() -> f64 {
    120.0
}

const fn default_seconds() -> f64 {
    2.0
}

impl Default for Synthetic {
    fn default() -> Self {
        Self { f0_hz: default_f0(), vowel: Vowel::A, seconds: default_seconds() }
    }
}

impl Synthetic {
    /// Why these settings can't be rendered, if they can't.
    #[must_use]
    pub fn problem(&self) -> Option<String> {
        if !(60.0..=500.0).contains(&self.f0_hz) {
            return Some(format!("f0_hz is {}; the synthetic voice speaks from 60 to 500 Hz", self.f0_hz));
        }
        if !(0.5..=30.0).contains(&self.seconds) {
            return Some(format!("seconds is {}; a clip is 0.5 to 30 s", self.seconds));
        }
        None
    }

    /// The voice, mono at 48 kHz, peaking at -6 dBFS.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    pub fn render(&self) -> Vec<f32> {
        const OPEN: f64 = 0.4;
        const CLOSING: f64 = 0.16;
        let len = (self.seconds.clamp(0.0, 30.0) * FS) as usize;
        let mut resonators = self.vowel.formants().map(|hz| Resonator::new(hz, 60.0 + hz * 0.02));
        let (mut phase, mut previous, mut scale, mut period) = (0.0f64, 0.0f64, 1.0f64, 0u64);
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            phase += self.f0_hz / FS * scale;
            if phase >= 1.0 {
                phase -= 1.0;
                period += 1;
                scale = 1.0 + 0.005 * noise(0x5EED, period);
            }
            // Rosenberg: a raised-cosine opening, a quarter-cosine closing, then closed.
            let pulse = if phase < OPEN {
                0.5 * (1.0 - (PI * phase / OPEN).cos())
            } else if phase < OPEN + CLOSING {
                (0.5 * PI * (phase - OPEN) / CLOSING).cos()
            } else {
                0.0
            };
            let mut x = (pulse - previous) * 20.0;
            previous = pulse;
            for r in &mut resonators {
                x = r.step(x);
            }
            out.push(x);
        }
        let peak = out.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let gain = if peak > 0.0 { 0.5 / peak } else { 0.0 };
        out.into_iter().map(|v| (v * gain) as f32).collect()
    }
}

/// A Klatt resonator: unity gain at DC, a peak at its centre.
#[derive(Copy, Clone, Debug)]
struct Resonator {
    a: f64,
    b: f64,
    c: f64,
    y1: f64,
    y2: f64,
}

impl Resonator {
    fn new(hz: f64, bandwidth: f64) -> Self {
        let c = -(-2.0 * PI * bandwidth / FS).exp();
        let b = 2.0 * (-PI * bandwidth / FS).exp() * (2.0 * PI * hz / FS).cos();
        Self { a: 1.0 - b - c, b, c, y1: 0.0, y2: 0.0 }
    }

    fn step(&mut self, x: f64) -> f64 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Why a WAV file can't be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct WavError(pub String);

/// `samples` (mono, 48 kHz) as a 32-bit float WAV file.
#[must_use]
pub fn wav(samples: &[f32]) -> Vec<u8> {
    let data = u32::try_from(samples.len() * 4).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + samples.len() * 4);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&48_000u32.to_le_bytes());
    out.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The samples of a 48 kHz WAV file (16- or 24-bit PCM, or 32-bit float), its channels averaged to mono.
///
/// # Errors
/// [`WavError`] naming what the file is, when it is not one of those.
pub fn read_wav(bytes: &[u8]) -> Result<Vec<f32>, WavError> {
    let bad = |what: &str| WavError(format!("not a WAV file this can read: {what}"));
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad("no RIFF/WAVE header"));
    }
    let u16_at = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (mut format, mut channels, mut rate, mut bits) = (None, 0usize, 0u32, 0u16);
    let mut at = 12;
    while let (Some(id), Some(size)) = (bytes.get(at..at + 4), u32_at(at + 4)) {
        let size = size as usize;
        let body = at + 8;
        match id {
            b"fmt " => {
                format = u16_at(body);
                channels = usize::from(u16_at(body + 2).unwrap_or(0));
                rate = u32_at(body + 4).unwrap_or(0);
                bits = u16_at(body + 14).unwrap_or(0);
            }
            b"data" => {
                let data = bytes.get(body..body + size).unwrap_or(&bytes[body.min(bytes.len())..]);
                if rate != 48_000 {
                    return Err(WavError(format!("the file is {rate} Hz; render takes 48 kHz audio")));
                }
                if channels == 0 {
                    return Err(bad("no channels"));
                }
                let frame = channels * usize::from(bits / 8);
                if frame == 0 {
                    return Err(bad("no sample size"));
                }
                #[allow(clippy::cast_precision_loss)]
                let sample = |b: &[u8]| -> Option<f32> {
                    match (format, bits) {
                        (Some(1), 16) => Some(f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0),
                        (Some(1), 24) => Some((i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0),
                        (Some(3), 32) => Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                        _ => None,
                    }
                };
                let width = usize::from(bits / 8);
                #[allow(clippy::cast_precision_loss)]
                let scale = 1.0 / channels as f32;
                return data
                    .chunks_exact(frame)
                    .map(|f| f.chunks_exact(width).map(sample).sum::<Option<f32>>().map(|s| s * scale))
                    .collect::<Option<Vec<f32>>>()
                    .ok_or_else(|| bad(&format!("format {format:?} at {bits} bits")));
            }
            _ => {}
        }
        at = body + size + (size & 1);
    }
    Err(bad("no data chunk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wav_reads_back_as_written() {
        let x: Vec<f32> = (0..1000).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
        assert_eq!(read_wav(&wav(&x)).unwrap(), x);
        assert!(read_wav(b"nope").is_err());
    }

    #[test]
    fn the_synthetic_voice_is_seeded_and_bounded() {
        let v = Synthetic::default().render();
        assert_eq!(v.len(), 96_000);
        assert_eq!(v, Synthetic::default().render());
        assert!(v.iter().all(|s| s.abs() <= 0.5 + 1e-6));
        assert!(Synthetic { f0_hz: 20.0, ..Synthetic::default() }.problem().is_some());
    }
}
