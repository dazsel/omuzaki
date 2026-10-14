//! Animation: ARKit-52 retarget, the life layer, blending and scores (spec v3 §20, §22; contract omuzaki-anim-api).
//!
//! Each subsystem is one module, added by its own task (one `mod` line here), so parallel PRs never edit the same code.
#![deny(missing_docs)]

pub mod gaze;
pub mod grammar;
pub mod hands;
pub mod life;
pub mod measure;
pub mod poses;
pub mod retarget;
pub mod score_templates;

omuzaki_core::define_link!();
