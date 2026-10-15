//! Contract for takes: the recorded inputs beside a recording or a replay clip (SH-01; ADR 0052; design in
//! `docs/design/takes.md`, section 5).
//!
//! A take's `<stem>.input` file is a header, then one self-describing checksummed chunk per frame period, then an
//! index and trailer. This crate holds only the types, constants and traits that every user of the format shares:
//! the codec is `omuzaki-take`, the scripted oracle is `omuzaki-take-mock`. Subsystems with live state add it to each
//! `State` with [`register_take_state!`] next to the code that owns it.
//!
//! Callers import by module path (ADR 0054 §2): `omuzaki_take_api::chunk::RecordKind`,
//! `omuzaki_take_api::state::StateRecord`.
#![deny(missing_docs)]

pub mod chunk;
pub mod error;
pub mod header;
pub mod record;
pub mod source;
pub mod state;

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

// Forwards to the scene contract, which forwards to the character contract (ADR 0015): an App that links the takes
// reaches every registration it builds on.
omuzaki_core::define_link! {
    omuzaki_stage_api::link();
}
