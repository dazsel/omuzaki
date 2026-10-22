//! Test doubles for the OS input contract (`omuzaki-input-api`, ADR 0060 task IN-2), so every consumer (input motion,
//! locomotion, the capture indicator, the binding editor, the ops) is built and tested before an OS backend exists.
//!
//! - [`FakeInput`](fake::FakeInput): the whole `InputHub` as one scripted value, on a `FakeClock`.
//! - [`ReplaySource`](replay::ReplaySource): recorded input as text, replayed as a `MotionFeed` in exactly the recorded
//!   (drained) order, bit-identically.
//! - [`conformance::run`]: the promises every `MotionFeed` keeps, checked against what went in.
#![deny(missing_docs)]

use omuzaki_input_api::binding::Intent;

pub mod conformance;
pub mod fake;
pub mod replay;

/// The `locomotion` preset (design §5): name, intent and the chord's label, which stays inside the hub.
pub const LOCOMOTION_PRESET: [(&str, Intent, &str); 7] = [
    ("walk forward", Intent::moving(0, 127), "W"),
    ("walk left", Intent::moving(-127, 0), "A"),
    ("walk back", Intent::moving(0, -127), "S"),
    ("walk right", Intent::moving(127, 0), "D"),
    ("jump", Intent::Jump, "Space"),
    ("run", Intent::Run, "Left Shift"),
    ("sit", Intent::Sit, "C"),
];
