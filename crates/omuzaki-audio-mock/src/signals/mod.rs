//! Deterministic test signals, generated in code (design section 9.1).
//!
//! Every generator that uses randomness takes a seed and draws from `omuzaki_core::rng::Rng`, so the same seed gives the
//! same samples. Nothing is read from disk and no recording of a real person is ever used. Signals are mono `Vec<f32>`
//! at the graph rate ([`SAMPLE_RATE`]), and every one has a fixed length, so a test can cut them into any block plan.
//!
//! | Item | What |
//! |---|---|
//! | [`voice`] | a synthetic voice: a Rosenberg glottal pulse train through four formant resonators, with F0 and vowel trajectories |
//! | [`clap`], [`plosive`] | decaying noise bursts |
//! | [`sine`], [`sine_sweep`] | a tone and a sweep |
//! | [`white_noise`], [`pink_noise`] | broadband noise |
//! | [`impulse`], [`click_train`] | single clicks, for latency measurements |
//! | [`changer`] | a delay plus a random timbre filter, for clap-test tests |
//!
//! "Bit-identical for a seed" holds on every target: the generators use only IEEE `+ - * /` and their own `sin`, `cos`,
//! `exp` and `ln` (`math.rs`), never the platform's math library, whose last bit differs between targets.

// Signals are `f32` made from `f64` arithmetic, and sample indices stay far below 2^52: the casts in this module are
// exact enough by construction.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

mod bursts;
mod changer;
mod math;
mod noise;
mod tones;
mod voice;

pub use bursts::{clap, plosive};
pub use changer::{Changer, changer};
pub use noise::{pink_noise, white_noise};
pub use omuzaki_audio_api::node::SAMPLE_RATE;
pub use tones::{Sweep, click_train, impulse, sine, sine_sweep};
pub use voice::{FormantFrequencies, Trajectory, VoiceSpec, Vowel, voice};

/// Samples in `seconds` at the graph rate, rounded to the nearest sample.
///
/// # Panics
/// Never: a negative or non-finite duration gives 0.
#[must_use]
pub fn samples_in(seconds: f64) -> usize {
    let wanted = (seconds * f64::from(SAMPLE_RATE)).round();
    if wanted.is_finite() && wanted > 0.0 {
        // Checked positive and finite just above; a test signal never nears `usize::MAX` samples.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            wanted as usize
        }
    } else {
        0
    }
}

/// The peak absolute value of `signal`.
#[must_use]
pub fn peak(signal: &[f32]) -> f32 {
    signal.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// `signal` scaled so its peak is `target` (unchanged when it is silent).
pub(crate) fn normalise(signal: &mut [f32], target: f32) {
    let current = peak(signal);
    if current > 0.0 {
        let scale = target / current;
        for s in signal.iter_mut() {
            *s *= scale;
        }
    }
}
