//! The Omuzaki desktop app's UI (docs/design/ux/).
//!
//! Each area is one module, added by its own task (one `mod` line here), so parallel PRs never edit the same code.
#![deny(missing_docs)]

pub mod aux_window;
pub mod ds;
pub mod live;
pub mod output_window;
pub mod shell;
pub mod wiring;

// Lets `register_story!` and `register_contrast!` record each registration for the link gate (ADR 0015).
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

// The stage registers the `perform.*` ops the Live workspace runs, and the animation crate the retarget and life
// layer the perform thread builds on, so the app links both (ADR 0015).
omuzaki_core::define_link! {
    omuzaki_anim::link();
    omuzaki_stage::link();
}
