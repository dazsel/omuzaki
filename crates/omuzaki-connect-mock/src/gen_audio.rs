//! Voice and music as 16-bit mono PCM WAV.
//!
//! - Voice: 16 kHz, a sine at 200 plus `seed mod 200` Hz, [`VOICE_SAMPLES_PER_CHAR`] samples (0.05 s) per character of the text, at
//!   most [`MAX_VOICE_SECONDS`].
//! - Music: 22.05 kHz, at most [`MAX_MUSIC_SECONDS`]; a two-note pattern for music, a decaying noise burst from the
//!   seed for a sound effect.

use std::f64::consts::TAU;

use omuzaki_connect_api::{Artifact, Capability, ConnectError, MediaKind, MusicKind, Output, Request};
use omuzaki_core::rng::Rng;

use crate::fake::{Generator, seed_of, wrong_kind};

/// The voice sample rate, Hz.
pub const VOICE_RATE: u32 = 16_000;
/// Samples of speech per character: 0.05 s at [`VOICE_RATE`].
pub const VOICE_SAMPLES_PER_CHAR: u32 = VOICE_RATE / 20;
/// The longest fake speech, seconds.
pub const MAX_VOICE_SECONDS: u32 = 10;
/// The music sample rate, Hz.
pub const MUSIC_RATE: u32 = 22_050;
/// The longest fake music, seconds.
pub const MAX_MUSIC_SECONDS: u32 = 2;
/// The model fake voices report.
pub const VOICE_MODEL: &str = "mock-voice@1";
/// The model fake music reports.
pub const MUSIC_MODEL: &str = "mock-music@1";

/// Loudness of the generated signals, as a fraction of full scale.
const AMPLITUDE: f64 = 0.3;

pub(crate) struct VoiceGen;

impl Generator for VoiceGen {
    fn capability(&self) -> Capability {
        Capability::Voice
    }

    fn make(&self, req: &Request, index: u32) -> Result<Output, ConnectError> {
        let Request::Voice(ask) = req else {
            return Err(wrong_kind(Capability::Voice, req));
        };
        let seed = seed_of(req, index);
        let chars = u32::try_from(ask.text.chars().count()).unwrap_or(u32::MAX);
        let samples = chars.saturating_mul(VOICE_SAMPLES_PER_CHAR).min(VOICE_RATE * MAX_VOICE_SECONDS);
        let hz = 200.0 + small(seed % 200);
        let pcm = (0..samples).map(|i| sine(hz, i, VOICE_RATE)).collect::<Vec<_>>();
        Ok(audio(wav(VOICE_RATE, &pcm), VOICE_MODEL, seed))
    }
}

pub(crate) struct MusicGen;

impl Generator for MusicGen {
    fn capability(&self) -> Capability {
        Capability::Music
    }

    fn make(&self, req: &Request, index: u32) -> Result<Output, ConnectError> {
        let Request::Music(ask) = req else {
            return Err(wrong_kind(Capability::Music, req));
        };
        let seed = seed_of(req, index);
        let samples = ask.seconds.min(MAX_MUSIC_SECONDS) * MUSIC_RATE;
        let pcm: Vec<i16> = match ask.kind {
            MusicKind::Music => {
                let root = 220.0 + small(seed % 100);
                (0..samples).map(|i| sine(if i < samples / 2 { root } else { root * 1.5 }, i, MUSIC_RATE)).collect()
            }
            MusicKind::Sfx => {
                let mut rng = Rng::new(seed);
                (0..samples)
                    .map(|i| {
                        let decay = (-f64::from(i) / f64::from(MUSIC_RATE) * 6.0).exp();
                        quantise((f64::from(rng.next_f32()) * 2.0 - 1.0) * decay)
                    })
                    .collect()
            }
        };
        Ok(audio(wav(MUSIC_RATE, &pcm), MUSIC_MODEL, seed))
    }
}

fn sine(hz: f64, i: u32, rate: u32) -> i16 {
    quantise((TAU * hz * f64::from(i) / f64::from(rate)).sin())
}

/// A value below 2^32 (a seed's remainder) as a float.
pub(crate) fn small(v: u64) -> f64 {
    f64::from(u32::try_from(v).unwrap_or(u32::MAX))
}

/// A sample in -1..=1 at [`AMPLITUDE`], as 16-bit PCM.
#[allow(clippy::cast_possible_truncation)] // clamped to ±AMPLITUDE of full scale first, so it fits an i16
fn quantise(v: f64) -> i16 {
    (v.clamp(-1.0, 1.0) * AMPLITUDE * f64::from(i16::MAX)).round() as i16
}

/// A 16-bit mono PCM WAV file.
fn wav(rate: u32, pcm: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(pcm.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn audio(bytes: Vec<u8>, model: &str, seed: u64) -> Output {
    Output::Asset(Artifact {
        bytes,
        media_type: "audio/wav".into(),
        kind: MediaKind::Audio,
        name: None,
        model: model.into(),
        seed: Some(seed),
        rights: None,
    })
}
