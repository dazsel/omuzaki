//! `fx.pitch`: our own low-latency pitch and formant shifter (design section 6.1, issues #917 and #1443).
//!
//! A pitch-synchronous design needs about one pitch period of look-ahead where a phase vocoder needs a whole FFT
//! window. The input is split into a source and a filter: [`analysis`] fits an LPC predictor per 4 ms hop
//! ([`lpc`]), inverse-filters the signal into its residual (the excitation), tracks F0 ([`f0`]) and places a mark at
//! each glottal closure ([`marks`]). [`synth`] moves the pitch by TD-PSOLA on the residual and the formants by scaling
//! the predictor's line spectral frequencies, then refilters. Pitch and formants are independent, so a pitch shift
//! keeps the voice's envelope (no chipmunk sound) and a formant shift keeps its pitch. The latency is
//! [`synth::LATENCY`], 912 samples (19 ms), declared exactly.

pub mod analysis;
pub mod f0;
pub mod lpc;
pub mod marks;
pub mod node;
pub mod synth;

pub use node::{Pitch, PitchKind};
