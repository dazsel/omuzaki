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
//! | [`sinks`] | [`sinks::png`] (one file per frame, crash-safe), [`sinks::wav`] (float WAV, header current each second) [`sinks::webm`] (VP9 with alpha, crash-safe clusters) and [`sinks::webm_mux`] (the same writer behind the `Muxer` trait) | works |
//! | [`vp9`] | VP9 through libvpx loaded at run time, behind the `Encoder` trait with its alpha as side data ([`vp9::alpha`]), and [`i420`]: straight RGBA to the colour and alpha pictures | works |
//! | [`record`] | [`settings`](record::settings), [`take`](record::take) naming and manifest, [`runtime`](record::runtime): the recorder output | works: `WebM` VP9 with alpha, or a PNG sequence |
//! | [`ops`] | [`record`](ops::record) (`record.start`, `stop`, `status`), [`replay`](ops::replay) (`record.replay_start`, `replay_stop`, `save_replay`, `replay_status`), [`output`](ops::output) (`output.kinds`, `list`, `start`, `stop`), [`window`](ops::window) | works |
//! | [`timeline`] | the [`AudioSource`](timeline::AudioSource) trait recording reads audio through | works |
//! | [`window`] | the clean window plan | works |
//! | [`settings_keys`] | the setting keys the stream reads | stub |
//! | [`replay`] | the replay buffer: the [`ring`](replay::ring), the disk [`segment`](replay::segment) store, the threaded [`runtime`](replay::runtime) that saves clips by stream copy, its [`settings`](replay::settings) and the stream service's [`control`](replay::control) | works |
//! | `ffmpeg` (feature `ffmpeg`) | `FFmpeg` encoders | stub |
//! | [`watchdog`] | the [`policy`](watchdog::policy) (health, deadlines, restart budget, log) and the [`supervisor`](watchdog::supervisor) running it over process and thread workers | works |
//! | `testing` (feature `testing`, or tests) | a fake encoder, a memory muxer and a scripted audio source | works |
//!
//! Benchmarks sit in `benches/` (`ring`, `replay`); the fake worker that the supervisor tests launch is `fake-worker`,
//! built from `src/bin/fake_worker.rs`.
#![deny(missing_docs)]

pub mod alpha;
pub mod encode_remote;
pub mod feed;
pub mod i420;
pub mod live_hold;
pub mod ops;
pub mod pacing;
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
