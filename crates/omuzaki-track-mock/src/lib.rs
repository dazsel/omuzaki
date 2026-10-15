//! Deterministic stand-ins for `omuzaki-track-api`: [`FakeCamera`] serves scripted or synthetic frames with exact
//! device timestamps, and [`FakeTracker`] replays scripted [`TrackFrame`](omuzaki_anim_api::TrackFrame)s at their
//! scripted stage times, signalling a lost face across gaps. Neither starts a thread, sleeps or reads a clock, so
//! tests that use them are exact and fast.
#![deny(missing_docs)]

mod camera;
mod tracker;

pub use camera::FakeCamera;
pub use tracker::FakeTracker;
