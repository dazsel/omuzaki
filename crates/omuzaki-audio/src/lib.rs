//! `omuzaki-audio`: the speech host (spec v3 ML-03), built from the spike.
//!
//! Microphone samples go in, speech events and transcripts come out:
//!
//! ```text
//!  audio callback --[AudioInput: lock-free ring]--> VAD thread --[bounded utterance queue]--> ASR thread
//!        (no allocation, no locks)          Silero VAD + segmenter        Whisper (whisper.cpp)
//!                                                       \------------ events (bounded) ------------/--> SpeechHost::next_event_timeout
//! ```
//!
//! - [`ring`]: the single-producer single-consumer sample ring behind [`AudioInput`]. Pushing never allocates, locks
//!   or blocks.
//! - [`vad`] and [`segmenter`]: Silero VAD through `ort` and the hysteresis state machine that turns its per-frame
//!   probabilities into speech start and end.
//! - [`asr`]: Whisper through `whisper-rs`, with the settings that bound latency (greedy, no context, token cap, no
//!   temperature fallback).
//! - [`host`]: [`SpeechHost`] runs both workers and owns the back-pressure policy.
//! - [`catalog`]: the pinned model files (URL, size, SHA-256) for the shared verified downloader.
//!
//! The host talks to its recogniser through the [`Recognizer`] trait and assumes nothing about CPU or GPU. The plan is that ASR
//! goes GPU-first, with `tiny.en` gated by the VAD as the CPU fallback and a stated CPU share. That backend
//! is not built here; the CPU [`Whisper`] below is the reference and the fallback's engine.
//!
//! Models are never committed. [`SpeechHost::open`] reports a missing file as [`SpeechError::ModelMissing`] naming
//! the catalogue entry to fetch.

// Signal-processing code: sample indices and times convert between integer and float all the time, and every value is
// orders of magnitude below where `f32` (2^24 samples is 17 minutes at 16 kHz) or `f64` lose exactness. Allowing the
// three cast lints for the crate keeps the arithmetic readable instead of burying it in `try_from` calls.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

pub mod asr;
pub mod catalog;
pub mod error;
pub mod host;
pub mod queue;
pub mod ring;
pub mod segmenter;
pub mod synth;
pub mod vad;

pub use asr::{Recognizer, Transcript, Whisper};
pub use error::SpeechError;
pub use host::{
    AudioInput, DEFAULT_HANGOVER_MS, HostConfig, HostStats, ModelPaths, SpeechEvent, SpeechHost, TranscriptDemand,
};
pub use segmenter::{Segment, Segmenter, SegmenterConfig};
pub use vad::{SileroVad, VoiceActivity};

/// The only sample rate the stack handles: both Silero VAD and Whisper take 16 kHz mono `f32`.
pub const SAMPLE_RATE: u32 = 16_000;
