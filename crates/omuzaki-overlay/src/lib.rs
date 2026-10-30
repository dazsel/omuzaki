//! The program compositor (spec v3 BR-13, BR-20, ST-04; `docs/design/overlays.md`): what the outputs carry on top of
//! the character, and how the program moves between scenes.
//!
//! - [`model`]: the widgets as the document keeps them (`overlay/widgets/<id>`): alerts, chat box, goal bar, timer
//!   (countdown, stopwatch, subathon) and text, with their layout and style
//! - [`live`]: each widget's live state, fed by the show's events, and what it draws
//! - [`paint`]: the CPU painter: a picture per widget, drawn again only when it changes, blended in with alpha
//! - [`transition`]: scene transitions (cut, fade, dip, wipe, slide, iris), blended in premultiplied alpha
//! - [`service`]: [`ProgramService`], which composes every output frame, and [`Tee`], which shows the show's events
//!   to it
//! - the ops (in `ops`): `overlay.kinds`, `list`, `add`, `set`, `remove`, `show`, `hide`, `alert`, `timer`, `goal`;
//!   `program.switch_scene`, `set_transition`, `transitions`, `status`
//!
//! **What this is not yet** (the design's later steps): widgets are drawn on the CPU into the frame the outputs read
//! back, so the studio's own viewport and capture window show the scene without them; web widgets for an OBS browser
//! source, captions and the AI bubble come next.
#![deny(missing_docs)]

omuzaki_core::define_link! {
    omuzaki_live_api::link();
}

pub mod live;
pub mod model;
mod ops;
pub mod paint;
pub mod service;
pub mod transition;

pub use service::{ProgramService, Tee, TimerAction};
