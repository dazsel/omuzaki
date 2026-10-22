//! The live audio engine (spec v3 §8, BR-22; ADR 0018; `docs/design/audio-api.md`): the implementation of
//! `omuzaki-audio-api`'s [`AudioEngine`](omuzaki_audio_api::AudioEngine) that carries the person's microphone into the
//! graph, the taps (recording, lip sync, recognition), the meters and the engine clock.
//!
//! | Module | What |
//! |---|---|
//! | [`backend`] | [`CpalBackend`](backend::CpalBackend): devices and streams through cpal |
//! | [`engine`] | [`LiveEngine`](engine::LiveEngine): the control side, bindings, streams, taps, feeds, status |
//! | `executor` | the real-time graph run on the clock device's callback |
//! | [`dsp`] | the 16 kHz decimator, the edge resampler, the meters |
//! | [`ops`] | `audio.devices`, `audio.mic`, `audio.status` |
//! | [`voice`] | lip sync: voice activity and visemes from the mic, as the `VoiceSource` service |
//!
//! The executor follows the contract's real-time rules: on the audio thread nothing locks (it takes its own state with
//! `try_lock`, which never waits), blocks, logs or frees; old graphs and closed taps go back to a housekeeping thread.

pub mod backend;
pub mod dsp;
pub mod engine;
mod executor;
pub mod ops;
pub mod voice;

pub use backend::CpalBackend;
pub use engine::LiveEngine;

omuzaki_core::define_link!();
