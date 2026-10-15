//! Errors of the update flow. All values; nothing panics.

use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::clock::Timestamp;
use crate::hold::HoldReason;
use crate::state::Watermark;
use crate::status::Unavailable;

/// Why an update step failed.
#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UpdateError {
    /// The feed could not be reached.
    #[error("offline")]
    Offline,
    /// The server answered with this status.
    #[error("http status {0}")]
    Http(u16),
    /// The manifest expired.
    #[error("manifest expired at {at:?}")]
    Expired {
        /// The `expires` time.
        at: Timestamp,
    },
    /// The manifest is older than one already seen.
    #[error("replayed manifest: have {have:?}, got {got:?}")]
    Replay {
        /// The stored watermark.
        have: Watermark,
        /// The manifest's.
        got: Watermark,
    },
    /// A signature did not verify.
    #[error("bad signature from key {key_id}")]
    BadSignature {
        /// The signing key's id.
        key_id: String,
    },
    /// The signing key is not in the app.
    #[error("unknown signing key")]
    UnknownKey,
    /// A file's hash differs from the manifest's.
    #[error("hash mismatch in {file}")]
    HashMismatch {
        /// The file.
        file: String,
    },
    /// A download or file passed its size cap.
    #[error("too large")]
    TooLarge,
    /// Not enough free space.
    #[error("disk full: {need} bytes needed")]
    DiskFull {
        /// Bytes needed.
        need: u64,
    },
    /// A delta's base version is not installed.
    #[error("delta base {missing} is missing")]
    DeltaBase {
        /// The base.
        missing: Version,
    },
    /// Something live holds the restart.
    #[error("busy: {0:?}")]
    Busy(Vec<HoldReason>),
    /// There is no previous version.
    #[error("nothing to roll back to")]
    NothingToRollBack,
    /// Updates are off for this install.
    #[error("updates unavailable: {0:?}")]
    Unavailable(Unavailable),
    /// A file operation failed.
    #[error("io: {0}")]
    Io(String),
}
