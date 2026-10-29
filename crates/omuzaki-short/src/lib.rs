//! Shorts (spec v3 §26): a short file, the lines of a script with a face, a shot and a caption each, rendered headless
//! and frame-exact to a finished video with its voice. It is how a person or an agent makes a talking-head short or a
//! reaction clip from a script in one call, and the quality loop's way to remake a reference short
//! (`docs/quality/reference-shorts.md`).
//!
//! The path, by module:
//!
//! | Module | Holds |
//! |---|---|
//! | [`def`] | the short file: [`ShortDef`](def::ShortDef), its lines, faces, shot words and caption styles |
//! | [`voice`] | the voice: a recording read and matched to the lines, lines said by a speaker, or silence timed from the words, with the mouth's frames |
//! | [`speech`] | the local text-to-speech (Kokoro through the model registry) as a speaker |
//! | [`timeline`] | when each line, face, shot and caption starts, and the measures against the reference bar |
//! | [`perform`] | the live performer (retarget, life, grammar, animator) stepped at 60 Hz on the short's clock |
//! | [`camera`] | the camera track: cuts, eases, punch-ins, pushes, dutch roll and handheld drift |
//! | [`caption`] | burned-in captions |
//! | [`render`] | the render loop |
//! | [`encode`] | VP9 and Opus into `WebM` |
//! | [`draft`] | `short.draft`'s rules: a plain script to a directed short |
//! | [`ops`] | `short.render`, `short.plan`, `short.draft` |
#![deny(missing_docs)]

pub mod camera;
pub mod caption;
pub mod def;
pub mod draft;
pub mod encode;
pub mod error;
pub mod ops;
pub mod perform;
pub mod render;
pub mod speech;
pub mod timeline;
pub mod voice;

pub use def::ShortDef;
pub use error::ShortError;

// Every registering crate the shorts build on, linked so a binary that links this crate keeps their registrations
// (ADR 0015). `build.rs` writes the calls from the dependencies' `registers` flags (ADR 0054 section 6).
omuzaki_core::define_link! {
    include!(concat!(env!("OUT_DIR"), "/link_calls.rs"));
}
