//! The Omuzaki desktop app's UI (docs/design/ux/).
//!
//! Each area is one module, added by its own task (one `mod` line here), so parallel PRs never edit the same code.
#![deny(missing_docs)]

pub mod ds;
pub mod panels;
pub mod shell;

// Lets `register_story!` and `register_contrast!` record each registration for the link gate (ADR 0015).
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

omuzaki_core::define_link! {
    omuzaki_char_api::link();
    omuzaki_render::link();
    omuzaki_stage::link();
}
