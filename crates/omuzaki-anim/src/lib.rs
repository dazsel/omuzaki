//! Animation: ARKit-52 retarget, the life layer, blending and scores (spec v3 §20, §22; contract omuzaki-anim-api).
//!
//! Each subsystem is one file or folder under `src/`, declared from the folder by the build script, so parallel PRs never edit the same code.
#![deny(missing_docs)]

// The crate's feature modules, declared from the folder by the build script (ADR 0054 §1).
include!(concat!(env!("OUT_DIR"), "/crate_mods.rs"));

pub use animator::{BusAnimator, BusAnimatorParams};
pub use triggers::{MAX_TRIGGERS, TriggerError, TriggerFades, TriggerMode, TriggerSlot, Triggers, expression_slot};

omuzaki_core::define_link!();
