//! Contract for the updater (SE-08; ADR 0035; design `docs/design/updates.md` sections 2, 3, 5 and 11).
//!
//! Traits and plain data only: no network, no file access, no thread, so the crate builds for `wasm32-unknown-unknown`.
//! `omuzaki-update` implements [`updater::Updater`]; `omuzaki-update-mock` is the test oracle; the launcher and the
//! updater share [`state::State`], [`layout::Layout`] and the [`disk::Disk`] seam from here because neither may depend on
//! the other.
//!
//! Callers import by module path (`omuzaki_update_api::status::UpdateStatus`, ADR 0054 §2); the one re-export at the root
//! is [`Version`], the `semver` type every module uses.
//!
//! | Module | What |
//! |---|---|
//! | [`updater`] | the `Updater` and `ModelCatalogue` services |
//! | [`release`] | `Release`, `Channel`, `PackageKind`, `Url` |
//! | [`status`] | `UpdateStatus` and the reasons it carries |
//! | [`error`] | `UpdateError` |
//! | [`hold`] | `Hold` and `HoldReason`: what keeps a restart from happening |
//! | [`event`] | `UpdateEvents`, a bounded queue per subscriber |
//! | [`progress`] | `Progress` for downloads |
//! | [`state`] | `state.toml` as `State`, and `Watermark` |
//! | [`layout`] | the install folder as `Layout` paths |
//! | [`disk`] | the `Disk` file operations |
//! | [`clock`] | `WallClock` and `Timestamp` |
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

pub use semver::Version;
