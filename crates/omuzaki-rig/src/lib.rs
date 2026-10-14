//! Rig: skeleton, humanoid pose solve, skinning matrices, spring bones and colliders (spec v3 §20; contracts omuzaki-anim-api, omuzaki-render-api).
//!
//! Each subsystem is one module, added by its own task (one `mod` line here), so parallel PRs never edit the same code.
#![deny(missing_docs)]

pub mod full;
pub mod ik;
pub mod pose;
pub mod skeleton;
pub mod skin;
pub mod spring;
