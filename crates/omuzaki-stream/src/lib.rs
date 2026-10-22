//! The recording and streaming implementation (spec v3 §9, §25; `docs/design/record.md`).
//!
//! This crate turns rendered frames and audio into files and live streams: the stream service every frame leaves
//! the app through, the PNG and WAV sinks, the recorder, the replay buffer, the encoders, the watchdog that keeps a
//! stream alive, and the output and record ops. The contracts it implements (encoders, muxers, packets, outputs) live in
//! `omuzaki-stream-api`; this crate is native only.
//!
//! # Module map
//!
//! | Module | Holds | State |
//! |---|---|---|
//! | [`service`] | [`StreamService`](service::StreamService): frames from the frame loop fanned out to every output, paced on each output's grid | works |
//! | [`alpha`] | the exact straight, premultiplied and flattened conversions of D9 | works |
//! | [`sinks`] | [`sinks::png`] (one file per frame, crash-safe), [`sinks::wav`] (float WAV, header current each second) and [`sinks::webm`] (VP9 with alpha, crash-safe clusters) | works |
//! | [`vp9`] | VP9 through libvpx loaded at run time, and [`i420`]: straight RGBA to the colour and alpha pictures | works |
//! | [`record`] | [`settings`](record::settings), [`take`](record::take) naming and manifest, [`runtime`](record::runtime): the recorder output | works: `WebM` VP9 with alpha, or a PNG sequence |
//! | [`ops`] | [`record`](ops::record) (`record.start`, `stop`, `status`), [`output`](ops::output) (`output.kinds`, `list`, `start`, `stop`), [`window`](ops::window) | works |
//! | [`timeline`] | the [`AudioSource`](timeline::AudioSource) trait recording reads audio through | works |
//! | [`window`] | the clean window plan | works |
//! | [`settings_keys`] | the setting keys the stream reads | stub |
//! | [`replay`] | the replay buffer | stub |
//! | `ffmpeg` (feature `ffmpeg`) | `FFmpeg` encoders | stub |
//! | [`watchdog`] | the supervisor | stub |
//! | `testing` (feature `testing`, or tests) | a fake encoder, a memory muxer and a scripted audio source | works |
//!
//! Benchmarks sit in `benches/` (`ring`, `replay`); the fake worker that the supervisor tests launch is `fake-worker`,
//! built from `src/bin/fake_worker.rs`.
#![deny(missing_docs)]

pub mod alpha;
pub mod i420;
pub mod ops;
pub mod record;
pub mod replay;
pub mod service;
pub mod settings_keys;
pub mod sinks;
pub mod timeline;
pub mod vp9;
pub mod watchdog;
pub mod window;

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

omuzaki_core::define_link!();
