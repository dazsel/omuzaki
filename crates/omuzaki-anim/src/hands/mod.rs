//! The hand-shape library (spec RG-09): open, fist, point, peace, heart, grip and thumbs-up, plus a relaxed rest, as
//! normalised humanoid finger poses.
//!
//! A [`HandShape`] is five [`FingerCurl`]s (thumb to little finger), each four angles. [`apply`] writes it onto the 15
//! finger bones of one hand in a [`HumanoidPose`](omuzaki_anim_api::HumanoidPose), blended in by a weight, so a cue, a
//! clip or (from phase 2, PF-06) hand tracking can all drive fingers through the same call. [`HandShape::from_curls`]
//! is the tracking entry point: one `0..=1` curl per finger.
//!
//! # Conventions
//!
//! Angles are **anatomical and side-neutral**: the same numbers make a left hand and a right hand that are mirror
//! images of each other, so one library serves both and `apply` does the mirroring. In radians in memory; the library
//! files are in degrees.
//! - `mcp`, `pip`, `dip`: flexion of the knuckle, the middle joint and the fingertip joint, positive toward the palm.
//! - `spread`: the swing of the whole finger in the plane of the palm, positive toward the thumb side. A fan of fingers
//!   therefore has the index positive and the little finger negative, and a V sign has the index positive and the
//!   middle finger negative.
//! - The thumb has three bones for four numbers: `spread` and `mcp` turn the metacarpal (its swing away from the
//!   index, and its flexion across the palm) and `pip` and `dip` bend the proximal and distal bones.
//!
//! # Axes
//!
//! The angles are written for the VRM T-pose hand, which is the rest of every character (ADR 0014). In the left hand's
//! T-pose the fingers point along +X, the palm faces -Y and the index finger is on the +Z side, so flexion is a turn
//! about -Z, spread a turn about -Y, and the right hand is the left one reflected through the centre plane, the same
//! rule as `HumanoidPose::mirrored`. The thumb is written for a thumb resting 45 degrees forward of the fingers in the
//! palm's plane, but VRM leaves the thumb's rest to the modeller, so [`HandRest::of`] measures where each thumb really
//! rests, once, at bind time, and [`apply`] carries the thumb from there to where the shape wants it. The same shape
//! then reads the same on every rig: a peace sign is a 20 degree V and a thumb closes across a fist whatever the
//! thumb's own rest.
//!
//! The library is data: one TOML file per shape in `library/hand-shapes/`, embedded with `include_str!` and parsed once
//! ([`HandLibrary::builtin`]). The query op `hand.list_shapes` lists it; applying a shape at runtime is an animator or
//! cue concern, not a document edit.
//!
//! Hot path: [`apply`] and [`HandShape::from_curls`] only do arithmetic on fixed arrays and read the rig's
//! [`HandRest`]. They never allocate or lock.

mod apply;
mod library;
mod ops;
mod rest;
mod shape;

pub use apply::{Side, apply};
pub(crate) use apply::{finger_targets, set_bone};
pub use library::HandLibrary;
pub use rest::{HandRest, HandRestError};
pub use shape::{Finger, FingerCurl, HandShape, HandShapeError, Joint};
