//! Contract for the updater (SE-08; ADR 0035; design `docs/design/updates.md` sections 2, 3, 5 and 11).
//!
//! Traits and plain data only: no network, no file access, no thread, so the crate builds for `wasm32-unknown-unknown`.
//! `omuzaki-update` implements [`Updater`]; `omuzaki-update-mock` is the test oracle; the launcher and the updater share
//! [`State`], [`Layout`] and the [`Disk`] seam from here because neither may depend on the other.
//!
//! | Module | What |
//! |---|---|
//! | [`updater`] | the [`Updater`] and [`ModelCatalogue`] services |
//! | [`release`] | [`Release`], [`Channel`], [`PackageKind`], [`Url`] |
//! | [`status`] | [`UpdateStatus`] and the reasons it carries |
//! | [`error`] | [`UpdateError`] |
//! | [`hold`] | [`Hold`] and [`HoldReason`]: what keeps a restart from happening |
//! | [`event`] | [`UpdateEvents`], a bounded queue per subscriber |
//! | [`progress`] | [`Progress`] for downloads |
//! | [`state`] | `state.toml` as [`State`], and [`Watermark`] |
//! | [`layout`] | the install folder as [`Layout`] paths |
//! | [`disk`] | the [`Disk`] file operations |
//! | [`clock`] | [`WallClock`] and [`Timestamp`] |
#![deny(missing_docs)]

pub mod clock;
pub mod disk;
pub mod error;
pub mod event;
pub mod hold;
pub mod layout;
pub mod progress;
pub mod release;
pub mod state;
pub mod status;
pub mod updater;

pub use clock::{Timestamp, WallClock};
pub use disk::{DirEntry, Disk};
pub use error::UpdateError;
pub use event::{EVENT_QUEUE_CAPACITY, EventSender, UpdateEvent, UpdateEvents, event_queue};
pub use hold::{Hold, HoldReason};
pub use layout::Layout;
pub use progress::Progress;
pub use release::{Channel, PackageKind, Release, Url};
pub use semver::Version;
pub use state::{State, StateError, Watermark};
pub use status::{CatalogueStatus, CheckFailure, RollbackReason, Unavailable, UpdateStatus};
pub use updater::{ModelCatalogue, Updater};
