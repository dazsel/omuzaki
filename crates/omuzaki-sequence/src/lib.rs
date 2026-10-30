//! Sequences (spec v3 §26 SH-02, SH-05): keyframed animation of whole scenes, shot by shot, rendered headless and
//! frame-exact to a video or to stills. A sequence file holds shots (each with a camera, an actor, props, toon volumes
//! and a backdrop) and motion-graphics layers composited in front of and behind them, every value a track of eased
//! keys. It is how a person or an agent animates a cut piece (a news segment with a stinger, a weather board, a gag
//! with smoke and a scorch) rather than a talking head.
//!
//! | Module | Holds |
//! |---|---|
//! | [`curve`] | the easing library, keys and tracks |
//! | [`def`] | the sequence file: [`SequenceDef`](def::SequenceDef), its shots, actors and props |
//! | [`volume`] | toon volumes: smoke, goo, cloud, steam, fire and dust as meshed, cel-shaded puffs |
//! | [`layer`] | motion-graphics layers: text, shapes, rings, bar charts, icons and speed lines |
//! | [`kinds`] | the prop kinds `news-desk`, `skyline`, `cloud-bank` and `card` |
//! | [`error`] | [`SequenceError`] |
#![deny(missing_docs)]

pub mod curve;
pub mod def;
pub mod error;
pub mod kinds;
pub mod layer;
pub mod volume;

pub use def::SequenceDef;
pub use error::SequenceError;

// Every registering crate sequences build on, linked so a binary that links this crate keeps their registrations
// (ADR 0015). `build.rs` writes the calls from the dependencies' `registers` flags (ADR 0054 section 6).
omuzaki_core::define_link! {
    include!(concat!(env!("OUT_DIR"), "/link_calls.rs"));
}
