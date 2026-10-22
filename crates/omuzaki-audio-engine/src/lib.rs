//! The live audio engine (spec v3 §8, BR-22; ADR 0018; `docs/design/audio-api.md`): the implementation of
//! `omuzaki-audio-api`'s [`AudioEngine`](omuzaki_audio_api::AudioEngine) that carries the person's microphone into the
//! graph, the taps (recording, lip sync, recognition), the meters and the engine clock.
//!
//! | Module | What |
//! |---|---|
//! | [`backend`] | [`CpalBackend`](backend::CpalBackend): devices and streams through cpal |
//! | [`engine`] | [`LiveEngine`](engine::LiveEngine): the control side, bindings, streams, taps, feeds, status |
//! | `executor` | the real-time graph run on the clock device's callback |
//! | `mix` | the mixer core both run: buses, chains, faders, sends, ducking, outputs |
//! | [`offline`] | [`OfflineMix`](offline::OfflineMix): the mixer core rendering offline (`OfflineRender`) |
//! | [`dsp`] | the 16 kHz decimator, the edge resampler, the hold-over delay |
//! | `meters` | peak, RMS, and on `program` short-term loudness, true peak and the limiter's reduction |
//! | [`ops`] | `audio.devices`, `audio.mic`, `audio.status`, `audio.meters` |
//! | [`bus_ops`] | `audio.bus.*`: buses and duck rules as document edits, under the egress rule |
//! | [`follow`] | keeps the live engine on the document's graph |
//!
//! The executor follows the contract's real-time rules: on the audio thread nothing locks (it takes its own state with
//! `try_lock`, which never waits), blocks, logs or frees; old graphs and closed taps go back to a housekeeping thread.

pub mod backend;
pub mod bus_ops;
pub mod dsp;
pub mod engine;
mod executor;
mod follow;
mod meters;
mod mix;
pub mod offline;
pub mod ops;
mod swap;

pub use backend::CpalBackend;
pub use engine::LiveEngine;
pub use follow::{follow, follow_reporting};
pub use offline::OfflineMix;

omuzaki_core::define_link!();
