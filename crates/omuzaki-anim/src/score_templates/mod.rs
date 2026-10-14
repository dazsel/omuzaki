//! Score templates (spec v3 §28): short performances (an intro wave, reactions, a kiss beat, an outro) written as
//! [`Score`] files in `library/scores/`, ready to drop on a timeline. Our own design targets, never measured from a
//! reference; a sound is only a `Marker` cue the person binds in an automation.

mod library;

pub use library::{ScoreTemplate, ScoreTemplateError, ScoreTemplates};
