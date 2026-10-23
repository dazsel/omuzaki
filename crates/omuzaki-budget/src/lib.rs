//! The frame-budget scheduler (spec v3 §8): every frame it reads the renderer's [`FrameReport`](omuzaki_render_api::FrameReport),
//! smooths the times, compares them with the budget and, when the frame is over budget for long enough, turns one
//! [`Quality`](omuzaki_render_api::Quality) knob down. When there has been room for long enough it turns the knob back
//! up. Every change is returned as a [`Change`] value for the app to log and show the user; this crate logs and draws
//! nothing.
//!
//! **Why a crate of its own.** §8 gives every system a budget, not only the renderer, so the scheduler is not a part of
//! the renderer. It depends on the render contract only, and its tests drive it with `omuzaki-render-mock`'s
//! `set_pass_cost`, so no GPU is involved.
//!
//! **The rules**, with their constants in [`scheduler`]:
//! - *Smoothing:* an exponential moving average of the measured time ([`scheduler::EMA_ALPHA`]).
//! - *Down:* the smoothed time over budget for [`scheduler::DOWN_SAMPLES`] samples in a row.
//! - *Up:* the smoothed time under [`scheduler::UP_MARGIN`] of the budget for [`scheduler::UP_SAMPLES`] in a row.
//! - *No oscillation:* the up threshold is below the down threshold (a dead band), and a step up that is undone within
//!   [`scheduler::FLAP_WINDOW`] frames doubles the wait for the next one (up to [`scheduler::MAX_HOLD`] times).
//! - *Output pressure:* the highest encoder load ([`Scheduler::observe_output`]) over
//!   [`scheduler::OUTPUT_LOAD_LIMIT`] steps down the same way, freeing CPU the encoder shares; an output's size and rate
//!   never change.
//! - *Ladder:* [`Step::LADDER`], the order in `docs/design/render-api.md`. The scheduler only caps `Quality` knobs and
//!   never touches the author's look.
//!
//! The face atlas is not a `Quality` knob (it is a texture the face system owns), so the scheduler only reports how the
//! face painter stands against its own budget ([`Scheduler::face_pressure`]); resizing the atlas is the face system's
//! follow-up.
#![deny(missing_docs)]

mod ladder;
pub mod scheduler;

pub use ladder::Step;
pub use scheduler::{Change, Direction, FacePressure, Scheduler};
