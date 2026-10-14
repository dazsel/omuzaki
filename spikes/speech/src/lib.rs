//! Spike: a GPL-free speech stack for `omuzaki-audio`.
//!
//! The spec (§9, §16) wanted `sherpa-onnx` for ASR and VAD, but its Rust crate statically links espeak-ng
//! (GPL-3.0-or-later). This crate proves the replacement: Silero VAD through `ort` ([`vad`]) and Whisper through
//! `whisper-rs` ([`asr`]), joined by a streaming [`segmenter`] and measured with [`metrics`] on audio that
//! [`synth`] generates or that the benchmark loads from [`wav`] files.

// This is signal-processing and statistics code: sample indices, frame counts and times are converted between integer and
// float all the time, and every value involved is orders of magnitude below where `f32` (2^24 samples = 17 minutes at
// 16 kHz would be the first to matter, and nothing here accumulates that way) or `f64` lose exactness. Allowing the three
// cast lints for the whole crate keeps the arithmetic readable instead of burying it in `try_from` calls.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

pub mod asr;
pub mod error;
pub mod metrics;
pub mod pipeline;
pub mod segmenter;
pub mod synth;
pub mod vad;
pub mod wav;

pub use error::SpeechError;

/// The only sample rate the stack handles: both Silero VAD and Whisper take 16 kHz mono `f32`.
pub const SAMPLE_RATE: u32 = 16_000;
