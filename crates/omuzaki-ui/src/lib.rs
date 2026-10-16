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

// The stage registers the `perform.*` and `trigger.*` ops the Live workspace runs, the stream crate the
// `output.window.*` ops its output switch runs, and the animation crate the retarget and life layer the perform thread
// builds on, so the app links all three, and the character crate, whose parts the kit character is built from (ADR 0015).
omuzaki_core::define_link! {
    omuzaki_anim::link();
    omuzaki_char::link();
    omuzaki_stage::link();
    omuzaki_stream::link();
}
