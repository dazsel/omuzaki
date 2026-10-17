//! The recording and streaming implementation (spec v3 §9, §25; `docs/design/record.md`).
//!
//! This crate turns rendered frames and audio into files and live streams: the A/V timeline, the PNG and WAV sinks,
//! the recorder, the replay buffer, the `FFmpeg` encoders and muxers, the watchdog that keeps a stream alive, and the
//! stream and record ops. The contracts it implements (encoders, muxers, packets, outputs) live in
//! `omuzaki-stream-api`; this crate is native only.
//!
//! # Module map
//!
//! The whole tree exists from the start, so parallel tasks never edit this file or `Cargo.toml`: each stub file starts
//! with a `//!` line naming its purpose, the table below names the task that fills each module, and each folder's
//! `mod.rs` lists its submodules.
//!
//! | Module | Holds | Filled by |
//! |---|---|---|
//! | [`timeline`] | the A/V timeline and the [`AudioSource`](timeline::AudioSource) trait | the A/V timeline task |
//! | [`alpha`] | alpha handling for the record formats | the A/V timeline task |
//! | [`sinks`] | [`sinks::png`] and [`sinks::wav`] | the PNG and WAV sinks task |
//! | [`record`] | [`settings`](record::settings), [`take`](record::take), [`runtime`](record::runtime) | the recording settings task, the recorder task |
//! | [`replay`] | [`ring`](replay::ring), [`segment`](replay::segment), [`runtime`](replay::runtime) | the rolling buffer task, the disk segment store task, the replay buffer task |
//! | `ffmpeg` (feature `ffmpeg`) | [`encode`](ffmpeg::encode), [`mux`](ffmpeg::mux), [`hw`](ffmpeg::hw), [`probe`](ffmpeg::probe) | the `FFmpeg` encoders task, the green-screen MP4 task |
//! | [`watchdog`] | [`policy`](watchdog::policy), [`supervisor`](watchdog::supervisor), [`launch`](watchdog::launch) | the watchdog policy task, the supervisor runtime task |
//! | [`window`] | the clean window plan | the clean window plan task |
//! | [`service`] | the stream service | the stream service task |
//! | [`ops`] | [`record`](ops::record), [`window`](ops::window) | the record ops task, the clean window plan task |
//! | [`settings_keys`] | the setting keys the recorder and the stream read | the record ops task |
//! | `testing` (feature `testing`, or tests) | [`encoder`](testing::encoder), [`muxer`](testing::muxer), [`source`](testing::source) | the testing kit task |
//!
//! The outputs of the spout, NDI and virtual camera tasks (`spout.rs`, `ndi.rs`, `vcam_*.rs`) are not part of this
//! crate's map.
//!
//! Benchmarks sit in `benches/` (`ring`, `replay`); the fake worker that the supervisor tests launch is `fake-worker`,
//! built from `src/bin/fake_worker.rs`.
#![deny(missing_docs)]

pub mod alpha;
pub mod ops;
pub mod record;
pub mod replay;
pub mod service;
pub mod settings_keys;
pub mod sinks;
pub mod timeline;
pub mod watchdog;
pub mod window;

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

omuzaki_core::define_link!();
