//! Co-speech gestures (spec AP-05, PF-24; #2349): a library of gesture phrases with their preparation and stroke, and
//! the scheduler that plays them so each stroke lands on a stressed syllable of the voice.
//!
//! - [`library`]: the phrase format and the built-in phrases (`library/gestures/*.toml`): beats, deictics,
//!   metaphorics, iconics, emblems and idle fidgets, each a preparation pose and a stroke pose per hand in a person's
//!   arm terms ([`ArmPose`](super::arm::ArmPose)), with hand shapes and a chest accent.
//! - [`scheduler`]: the talk loop. While the voice speaks it picks a phrase, prepares it, holds the pre-stroke pose
//!   until a stressed syllable and strikes on it; a regular rhythm of stresses lets it strike early, so the stroke
//!   lands *on* the predicted syllable rather than after it. Between phrases the hands wait at the talk-rest pose;
//!   when the voice stops they go back to the body's own rest, and idle fidgets come every few seconds.

pub mod library;
pub mod scheduler;

pub use library::{Gesture, GestureError, GestureKind, GestureLibrary, Hands, Key};
pub use scheduler::{GestureScheduler, Phase, StrokeLog};
