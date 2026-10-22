//! OS input for motion (ADR 0060, `docs/design/input-motion.md`): the hub behind `omuzaki-input-api`.
//!
//! Capture is off by default and starts only when a person asks while an installed indicator is drawn (§4). Each OS
//! backend reads on its own capture thread and hands raw events to its sink, the one place key identities exist
//! (`normalise`, §3): keys become zones, chords are matched against the binding table, and the results go to the
//! backend's own single-producer rings, stamped with the epochs they were read in. The feeds drain those rings on the
//! frame thread and the dispatcher; a pause, a stop or a backend leaving closes an epoch, and every feed ends what it
//! holds on seeing it (§4, §5).
//!
//! Each backend is a folder under `src/`, gated inside by `#![cfg(target_os = …)]` and registered next to its code with
//! `register_input_backend!`; the build script declares the folders, so adding one edits nothing else.
#![deny(missing_docs)]

// The crate's modules, declared from the folder by the build script (ADR 0054 §1).
include!(concat!(env!("OUT_DIR"), "/crate_mods.rs"));

pub use hub::{Hub, HubConfig};

#[cfg(feature = "test-hooks")]
pub use test_hooks::{FakeProcesses, Injector, TestBackend, keys_in_zone};

omuzaki_core::define_link!();
