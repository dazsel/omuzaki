//! The perform loop (PF-24; spec v3 §8, §10, §22): a [`FrameSource`](omuzaki_anim_api::FrameSource) feeds the
//! retarget, the layer stack, the life layer and the animator at a fixed 60 Hz step, and a dead source never freezes
//! the face.
//!
//! This is the real counterpart of `MockPerformer` in `omuzaki-anim-mock`. Every stage is a trait object, so the App
//! wires in `ArkitRetarget` and `Life` and tests swap in mocks. It runs on the perform thread; the hand-off from the
//! tracker thread is the source's job (a newest-frame-wins mailbox).

pub mod frame;
pub mod host;
pub mod input;
pub mod live;
mod performer;
pub mod settings;
pub mod triggers;

pub use performer::{
    MAX_STEPS_PER_ADVANCE, PerformOutput, Performer, PerformerParts, STALE_SOURCE_S, STEP_HZ, Tracking,
};
