//! The voice the face hears: voice activity, loudness and the five lip-sync visemes, per analysis frame (spec v3
//! FE-08, the "Audio and visemes" contract between Broadcast, Tracking and Face).
//!
//! An analyser reads the [`VoiceUse::Mouth`](crate::tap::VoiceUse::Mouth) voice tap (16 kHz mono, silent when the
//! person mutes) and publishes a [`VoiceFrame`] every [`VOICE_HOP_SECONDS`]. The stage reads the latest frame each
//! tick through [`VoiceSource`] and turns the weights into the face's viseme expressions; nothing here knows about the
//! face. The viseme names are the VRM presets, the same ids as the face's viseme expressions, so a lip-sync source,
//! the painted face and a VRM export agree without a mapping table.

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many visemes a frame weighs.
pub const VISEME_COUNT: usize = 5;

/// The visemes, in [`VoiceFrame::visemes`] order: the VRM preset names (and the face's viseme expression ids).
pub const VISEMES: [&str; VISEME_COUNT] = ["aa", "ih", "ou", "ee", "oh"];

/// The time between voice frames, in seconds.
pub const VOICE_HOP_SECONDS: f64 = 0.01;

/// What the voice was doing over one analysis window.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VoiceFrame {
    /// The engine-clock index (48 kHz) of the window's centre, after the tap's capture offset: the moment the sound
    /// was made, so the face can line up with the video frame captured at the same time.
    pub sample: u64,
    /// The same moment on the pipeline clock.
    pub time: Seconds,
    /// Whether the person is speaking (with a short hold, so the mouth does not snap shut between syllables).
    pub active: bool,
    /// The window's level in dBFS; `-inf` for digital silence.
    pub level_db: f32,
    /// How open the mouth should be from loudness alone, `0` to `1`: a viseme-free fallback for faces with no
    /// viseme expressions.
    pub open: f32,
    /// The weight of each of [`VISEMES`], `0` to `1`, already scaled by `open`; all zero while not `active`.
    pub visemes: [f32; VISEME_COUNT],
}

impl VoiceFrame {
    /// A frame of silence at `sample` / `time`.
    #[must_use]
    pub fn silent(sample: u64, time: Seconds) -> Self {
        Self { sample, time, active: false, level_db: f32::NEG_INFINITY, open: 0.0, visemes: [0.0; VISEME_COUNT] }
    }

    /// The weight of the viseme named `name` (a [`VISEMES`] entry), `None` for another name.
    #[must_use]
    pub fn weight(&self, name: &str) -> Option<f32> {
        VISEMES.iter().position(|v| *v == name).map(|i| self.visemes[i])
    }
}

/// Where the stage reads the voice: shared as a service (`Services::get::<dyn VoiceSource>()`; `Send + Sync`, so a
/// `Service` by the blanket impl).
pub trait VoiceSource: Send + Sync {
    /// The most recent frame, `None` before the analyser has heard anything (no microphone yet).
    fn latest(&self) -> Option<VoiceFrame>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_frame_weighs_nothing_and_names_resolve() {
        let f = VoiceFrame::silent(48_000, Seconds(1.0));
        assert!(!f.active);
        assert_eq!(f.weight("aa"), Some(0.0));
        assert_eq!(f.weight("blink"), None);
    }
}
