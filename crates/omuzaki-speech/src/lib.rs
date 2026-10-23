//! `omuzaki-speech`: the speech host (spec v3 ML-03) and the speech slots (ML-01, ADR 0017).
//!
//! Microphone samples go in, speech events and transcripts come out:
//!
//! ```text
//!  audio callback --[AudioInput: lock-free ring]--> VAD thread --[bounded utterance queue]--> ASR thread
//!  (or the listen tap)                        vad@1 session + segmenter      asr@1 session
//!        (no allocation, no locks)
//!                                                       \------------ events (bounded) ------------/--> SpeechHost::next_event_timeout
//! ```
//!
//! - [`slots`]: `vad@1` and `asr@1`, their signatures and the evaluations every model in them passes, on public-domain
//!   speech pinned like model files.
//! - [`whisper_runtime`]: the `whisper-ggml` runtime, Whisper's ggml files through whisper.cpp, presenting `asr@1`.
//!   Silero VAD runs on the `onnx` runtime (`omuzaki-ai-ort`); both models ship as `models/*.model.toml`.
//! - [`vad`] and [`segmenter`]: voice activity on a `vad@1` session and the hysteresis state machine that turns its
//!   per-frame probabilities into speech start and end.
//! - [`asr`]: speech to text on an `asr@1` session.
//! - [`host`]: [`SpeechHost`] runs both workers and owns the back-pressure policy. It reports `Held` (the barge-in,
//!   three speech frames in a row), the mute flag, and which utterances were spoken privately (`CohostOnly`).
//! - [`ears`]: [`SpeechEars`], the host as the AI performer hears it, dropping its own echo.
//! - [`commands`]: voice commands (AP-10): the phrase matcher, the [`CommandListener`] that turns the person's raw-mic
//!   transcripts into `voice.command` events, and the opt-in `voice_commands.*` ops.
//! - [`tts`]: [`TtsHost`], the performer's voice: a synth thread and a pump writing the `ai_voice` feeds, cut at once,
//!   resumed from a cache, pitched by the declared rate.
//! - [`voices`]: the voice catalog, the consent gate every cloned voice passes, the public-figure warning and the
//!   `voice.get`, `voice.list`, `voice.assign` and `voice.preview` ops.
//! - [`slots::tts`] and [`kokoro_runtime`]: `tts@1` (text to speech, checked by a round trip through `asr@1`) and the
//!   `kokoro` runtime, Kokoro-82M's ONNX graph behind [`g2p`], our own GPL-free English G2P over the gold lexicons.
//!
//! The host talks to its models through registry sessions and assumes nothing about CPU or GPU: a slot session follows
//! the person's choice of model and falls back to the shipped one on its own. Models are never committed; the registry
//! downloads and verifies them.

// Signal-processing code: sample indices and times convert between integer and float all the time, and every value is
// orders of magnitude below where `f32` (2^24 samples is 17 minutes at 16 kHz) or `f64` lose exactness. Allowing the
// three cast lints for the crate keeps the arithmetic readable instead of burying it in `try_from` calls.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
// Names of recordings and products (LibriVox, Silero, Whisper) are prose here, not code.
#![allow(clippy::doc_markdown)]

pub mod asr;
pub mod commands;
pub mod ears;
pub mod error;
pub mod g2p;
pub mod host;
pub mod kokoro_runtime;
pub mod queue;
pub mod segmenter;
pub mod slots;
pub mod synth;
pub mod tts;
pub mod vad;
pub mod voices;
pub mod whisper_runtime;

pub use asr::{AsrCall, Recognizer, Transcript, Whisper};
pub use commands::{CommandListener, CommandStatus, PhraseList, VoiceCommand, VoiceCommands, match_transcript};
pub use ears::{EchoLog, SpeechEars};
pub use error::SpeechError;
pub use host::{
    AudioInput, DEFAULT_HANGOVER_MS, HostConfig, HostStats, SpeechEvent, SpeechEvents, SpeechHost, TranscriptDemand,
    TranscriptSwitch,
};
pub use kokoro_runtime::Kokoro;
pub use segmenter::{Segment, Segmenter, SegmenterConfig};
pub use tts::{TtsHost, TtsStats, TtsVoice};
pub use vad::{SileroVad, VadStream, VoiceActivity};
pub use whisper_runtime::WhisperGgml;

/// The only sample rate the stack handles: both voice activity and speech recognition take 16 kHz mono `f32`.
pub const SAMPLE_RATE: u32 = 16_000;

omuzaki_ai_api::register_model!("silero-vad", include_str!("../models/silero-vad.model.toml"));
omuzaki_ai_api::register_model!("whisper-tiny-en", include_str!("../models/whisper-tiny-en.model.toml"));
omuzaki_ai_api::register_model!("whisper-base-en", include_str!("../models/whisper-base-en.model.toml"));
omuzaki_ai_api::register_model!("whisper-base-en-q5-1", include_str!("../models/whisper-base-en-q5-1.model.toml"));
omuzaki_ai_api::register_model!("whisper-small-en", include_str!("../models/whisper-small-en.model.toml"));
omuzaki_ai_api::register_model!("whisper-small-en-q5-1", include_str!("../models/whisper-small-en-q5-1.model.toml"));
omuzaki_ai_api::register_model!(
    "whisper-large-v3-turbo-q5-0",
    include_str!("../models/whisper-large-v3-turbo-q5-0.model.toml")
);
omuzaki_ai_api::register_model!("kokoro-82m", include_str!("../models/kokoro-82m.model.toml"));

omuzaki_core::define_link! {
    omuzaki_char_api::link();
    omuzaki_live_api::link();
    omuzaki_performer_api::link();
}
