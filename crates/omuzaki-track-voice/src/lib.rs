//! Voice-driven mouth and cues (spec v3 FE-08 lip sync from audio, PF-08 voice cues): the person's microphone moves
//! the character's mouth and feeds the life layer's speech cues, with no model to download.
//!
//! ```text
//!  audio thread ──tap ring──▶ VoiceListener thread: VoiceAnalyzer ──voice mailbox──▶ perform thread: LipSync
//!  (audio-api, Voice(Mouth))   level, voicing, onsets, LPC formants   (newest wins)     audio layer + Cues
//! ```
//!
//! | Module | What it does |
//! |---|---|
//! | [`ai_mouth`] | [`MouthDrive`]: the AI performer's mouth from its TTS lead through `voice_to_face@1`, released on the play log |
//! | [`analyzer`] | [`VoiceAnalyzer`]: samples at any rate to a [`VoiceFrame`] every 10 ms |
//! | [`onset`] | [`OnsetDetector`]: syllable onsets from the three-band onset strength, peak-picked adaptively |
//! | [`cues`] | [`CueExtractor`]: the life layer's loudness and onset cues from the `cues` tap, in performance time; [`cue_mailbox`] and [`CueListener`] |
//! | [`lead`] | [`LeadAnalysis`]: the AI performer's TTS voice through the same analyser, released as its audio plays; with the line's phonemes, its mouth from the aligned phones |
//! | [`viseme`] | the five visemes and the [`VowelProfile`] that maps formants onto them |
//! | [`mailbox`] | [`voice_mailbox`]: the wait-free newest-wins hand-off to the perform thread |
//! | [`lipsync`] | [`LipSync`]: frames to the audio layer, through the character's viseme expressions, and the cues |
//! | [`listener`] | [`VoiceListener`]: the analyser's thread over an audio-api tap ring |
//! | [`phoneme`] | [`PhonemeMouth`]: the mouth from known phones and their times (an AI line, a short's voice), closures included |
//! | [`align`] | [`align()`](align::align): the times of a line's phones in its audio, by dynamic programming over the analyser's frames |
//! | [`eval`] | the lip-sync evaluation: vowel accuracy, closures, silence and A/V offset against the phones really said |
//! | [`synth`] | a deterministic formant synthesiser for tests and evidence, with synthetic lines and their exact phones |
//!
//! The analysis is classic signal processing (the technique of formant-based lip sync, as in uLipSync's MFCC or
//! Rhubarb's phonetic recogniser, at a fraction of the cost): it needs no model, runs in well under a percent of a
//! core, and its per-person vowel calibration (PF-08) is one [`VowelProfile`]. A learned voice-to-face model (ML-05,
//! ML-07) can replace the visemes later behind the same audio layer.
#![deny(missing_docs)]
// Product names (uLipSync, Rhubarb, VRM) are prose here, not code.
#![allow(clippy::doc_markdown)]

pub mod ai_mouth;
pub mod align;
pub mod analyzer;
pub mod cues;
pub mod eval;
pub mod lead;
pub mod lipsync;
pub mod listener;
pub mod mailbox;
pub mod onset;
pub mod phoneme;
pub mod synth;
pub mod viseme;

pub use ai_mouth::{MouthConfig, MouthDrive, MouthHandle, MouthStats, MouthThread};
pub use analyzer::{VoiceAnalyzer, VoiceError, VoiceFrame};
pub use cues::{CueExtractor, CueFrame, CueListener, CueReader, CueWriter, cue_mailbox};
pub use lead::LeadAnalysis;
pub use lipsync::{LipSync, LipSyncConfig};
pub use listener::VoiceListener;
pub use mailbox::{VoiceReader, VoiceWriter, voice_mailbox};
pub use onset::OnsetDetector;
pub use phoneme::{MouthFrame, Phone, PhoneSpan, PhonemeMouth, VisemeShapes};
pub use viseme::{Viseme, VowelProfile};

omuzaki_core::define_link! {
    omuzaki_performer_api::link();
}
