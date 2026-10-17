//! Imported avatars at run time (spec v3 §27 EX-12, §8, §10; design: `docs/design/byom.md` D3; ADR 0033).
//!
//! - [`evaluator`]: [`ExpressionEvaluator`](evaluator::ExpressionEvaluator), which runs after the anim blend and before
//!   the renderer, on the frame thread, and turns the channel buffer into morph weights and material bind deltas
//! - [`buffers`]: what it writes, [`MorphWeights`](buffers::MorphWeights) and [`BindDeltas`](buffers::BindDeltas), sized
//!   once per avatar
#![deny(missing_docs)]

pub mod buffers;
pub mod evaluator;

// No registrations of its own; forwards to the contracts' (ADR 0015).
omuzaki_core::define_link! {
    omuzaki_avatar_api::link();
    omuzaki_char_api::link();
}
