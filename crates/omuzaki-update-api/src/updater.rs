//! The services: [`Updater`] and [`ModelCatalogue`].

use crate::error::UpdateError;
use crate::event::UpdateEvents;
use crate::hold::{Hold, HoldReason};
use crate::progress::Progress;
use crate::release::{Channel, Release};
use crate::status::{CatalogueStatus, UpdateStatus};

/// The update flow. `check` and `download` run on the updater's worker thread, never the frame, audio or executor
/// threads. Install and rollback are person-only (design sections 6 and 12).
pub trait Updater: Send + Sync {
    /// Where the flow stands.
    fn status(&self) -> UpdateStatus;

    /// Fetches and verifies the manifest; `Some` when a newer version is offered.
    ///
    /// # Errors
    /// [`UpdateError::Offline`], `Http`, `Expired`, `Replay`, `BadSignature` or `UnknownKey`.
    fn check(&self, channel: Channel) -> Result<Option<Release>, UpdateError>;

    /// Downloads and verifies the package, reporting to `progress`, which can cancel.
    ///
    /// # Errors
    /// `HashMismatch`, `TooLarge`, `DiskFull`, `DeltaBase`, `Offline`, `Http` or `Io`.
    fn download(&self, release: &Release, progress: &dyn Progress) -> Result<(), UpdateError>;

    /// Unpacks a downloaded package beside the running version. Applied at the next start, never hot-swapped.
    /// Idempotent; verifies again before unpacking.
    ///
    /// # Errors
    /// `HashMismatch`, `DiskFull` or `Io`.
    fn stage(&self, release: &Release) -> Result<(), UpdateError>;

    /// Points back at the previous version. Takes effect at the next start.
    ///
    /// # Errors
    /// [`UpdateError::NothingToRollBack`] without a previous version.
    fn rollback(&self) -> Result<(), UpdateError>;

    /// A live subsystem takes one while it must not be interrupted; dropping it releases.
    fn hold(&self, why: HoldReason) -> Hold;

    /// The app calls this once the main window is up. Idempotent; writes the marker the launcher reads.
    fn healthy(&self);

    /// A new bounded queue of events.
    fn subscribe(&self) -> UpdateEvents;

    /// Whether a restart may happen now.
    ///
    /// # Errors
    /// [`UpdateError::Busy`] with the reasons of every hold, in the order they were taken.
    fn restart_allowed(&self) -> Result<(), UpdateError>;
}

/// The signed model catalogue, refreshed by `update.check` or the Models page. A seam of its own so this crate
/// carries no `omuzaki-ai-api` types.
pub trait ModelCatalogue: Send + Sync {
    /// Fetches and verifies the catalogue.
    ///
    /// # Errors
    /// The same trust failures as [`Updater::check`].
    fn refresh(&self) -> Result<CatalogueStatus, UpdateError>;

    /// The last result.
    fn status(&self) -> CatalogueStatus;
}
