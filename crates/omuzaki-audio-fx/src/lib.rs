//! The voice effects (spec v3 §25 BR-24, ADR 0018, design `docs/design/audio-api.md` section 6), each a registered
//! audio node kind that the engine places in a bus's effect chain.
//!
//! | Kind | Module | What |
//! |---|---|---|
//! | `fx.eq` | [`eq`] | up to eight RBJ bands |
//! | `fx.gate` | [`gate`] | a noise gate with hysteresis, hold and a side-chain high-pass |
//! | `fx.compressor` | [`compressor`] | a feed-forward soft-knee compressor |
//! | `fx.robot` | [`robot`] | a ring modulator into a feedback comb |
//! | `fx.radio` | [`radio`] | telephone and radio voices |
//! | `fx.limiter` | [`limiter`] | the program bus's -1 dBTP true-peak safety limiter |
//! | `fx.pitch` | [`pitch`] | our low-latency pitch and formant shifter |
//!
//! Beside the kinds: [`chain`] runs a preset's chain outside the engine, [`presets`] holds the built-in presets,
//! [`ops`] holds the `audio.fx.*` ops, and [`clip`] and [`measure`] are what their render jobs play and report.
//!
//! Every node follows the rules of `omuzaki_audio_api::node`: its latency is declared exactly, its output does not
//! depend on how the blocks are cut (anything periodic is aligned to the graph clock), and its audio-thread methods
//! never allocate. `tests/conformance.rs` holds every kind to them with the suite in `omuzaki-audio-mock`.
#![deny(missing_docs)]
// DSP: sample and tap indexes (far below 2^52) become f64 and back, audio is f32, and filter maths keeps the names of
// its textbooks (`a`, `b`, `k`, `x`, `y`).
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

pub mod biquad;
pub mod chain;
pub mod clip;
pub mod compressor;
mod dsp;
pub mod eq;
pub mod gate;
pub mod limiter;
pub mod measure;
pub mod ops;
pub mod pitch;
pub mod presets;
pub mod radio;
pub mod robot;

omuzaki_core::define_link!();
