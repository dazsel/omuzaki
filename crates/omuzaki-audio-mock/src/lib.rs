//! The test oracle for the audio engine contract (`omuzaki-audio-api`; spec v3 §39 rule 6, §9; ADR 0018).
//!
//! | Item | What |
//! |---|---|
//! | [`OfflineEngine`] | the single-threaded reference [`OfflineRender`](omuzaki_audio_api::OfflineRender), written to be obviously correct rather than fast |
//! | [`MockEngine`] | an [`AudioEngine`](omuzaki_audio_api::AudioEngine) for op tests: it stores the graph, records the live setters, and runs it block by block from [`MockEngine::advance`] |
//! | [`NullKind`] | the `mock.null` node kind: a configurable-latency pass-through with one gain parameter |
//!
//! Both engines evaluate the graph with the same reference mixer. It uses the shared recurrences of
//! `omuzaki_audio_api::semantics` (fader ramps, the duck envelope) and implements routing, ordering, sends, channel
//! conversion and taps on its own, so the real engine in `omuzaki-audio` is compared with an independent oracle
//! (design section 3.6).
//!
//! Not here yet: the fake backend and the node conformance suite (`src/backend/` and `src/conformance/`).
#![deny(missing_docs)]

mod engine;
mod latency;
mod mixer;
mod null;
mod offline;
pub mod signals;
mod taps;

pub use engine::{MockEngine, mock_devices};
pub use null::{MAX_NULL_LATENCY, NULL_KIND_ID, NullKind};
pub use offline::{Change, OfflineEngine, TimedChange};

omuzaki_core::define_link!();
