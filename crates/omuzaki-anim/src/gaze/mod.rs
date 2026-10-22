//! Gaze targets: where a character looks, solved into the eight `eyeLook*` channels (spec FE-07).
//!
//! A [`GazeTarget`] names what to look at (the camera, a point, another entity, or nothing). The stage resolves it to a
//! world position ([`GazeTarget::resolve`]); [`GazeSolver::step`] turns that position into per-eye angles, eases them
//! the way a real eye travels, and writes them to the bus. Run it **before** the life layer: the life layer adds its
//! saccades and drift on top of whatever the bus already looks at.
//!
//! The maths ports the camera follow of the web prototype (`face.js` `update`) and adds per-eye convergence: each eye
//! aims from its own position, so near targets converge and far ones go parallel.
//!
//! [`GazeCoordinator`] runs the solver with the head: for a look at a new target the eyes go first and the head follows
//! 120 to 200 ms later, only as far as the eyes can't reach, and a `Look` cue holds a target for a while.
//! [`highlight_shift`] gives the painter where the eyes' highlights slide as the camera moves round the face.

mod coordinate;
mod highlight;
mod solve;
mod target;

pub use coordinate::{GazeCoordinator, GazeTurn, HeadShare};
pub use highlight::{HIGHLIGHT_MAX_X, HIGHLIGHT_MAX_Y, HIGHLIGHT_PER_RADIAN, highlight_shift};
pub use solve::{
    DRIVEN_RATE, FOLLOW_RATE, GazeFrame, GazeSolver, HALF_EYE_SPACING, X_RADIANS_PER_UNIT, Y_RADIANS_PER_UNIT,
};
pub use target::{EntityId, GazeKind, GazeScene, GazeTarget, Resolved};
