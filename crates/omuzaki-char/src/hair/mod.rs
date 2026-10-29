//! The `hair` part kind: generators, tube geometry, physics, colour and bake.
//!
//! The generators live in `gen/`; `gen` is a reserved word in edition 2024, so the module is `r#gen`.

pub mod bake;
pub mod colour;
pub mod compile;
pub mod ears;
pub mod edit;
pub mod r#gen;
pub mod part;
pub mod physics;
pub mod reach;
pub mod tie;
pub mod trace;
pub mod tube;
pub mod wisp;
