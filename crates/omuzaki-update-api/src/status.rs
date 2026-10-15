//! The updater's status, as the UI and the `update.*` ops show it.

use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::clock::Timestamp;
use crate::hold::HoldReason;
use crate::release::Release;

/// Why updates are off.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    /// A package manager owns the install.
    Managed,
    /// The install root cannot be written.
    ReadOnlyRoot,
    /// A debug or development build.
    DevBuild,
}

/// Why a check failed. Shown for explicit checks only.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckFailure {
    /// The manifest is past its `expires`.
    Expired,
    /// The manifest's sequence went backwards.
    Replay,
    /// A signature did not verify.
    BadSignature,
    /// The feed could not be reached.
    Offline,
}

/// Why a version was rolled back.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RollbackReason {
    /// The launcher's boot guard: the version died twice before the UI was ready.
    FailedToStartTwice,
    /// The person asked.
    Manual,
}

/// Where the update flow stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UpdateStatus {
    /// Updates are off for this install.
    Unavailable {
        /// Why.
        reason: Unavailable,
    },
    /// Nothing to do.
    Idle {
        /// When the last check finished.
        last_checked: Option<Timestamp>,
    },
    /// A newer version is offered.
    Available {
        /// The offer.
        release: Release,
    },
    /// A package is downloading.
    Downloading {
        /// The version being fetched.
        version: Version,
        /// 0 to 100.
        percent: u8,
    },
    /// Verified and unpacked; applied at the next start.
    Staged {
        /// The staged version.
        version: Version,
        /// Holds that would stop a restart now.
        deferred_by: Vec<HoldReason>,
    },
    /// The last explicit check failed.
    CheckFailed {
        /// When.
        at: Timestamp,
        /// Why.
        reason: CheckFailure,
    },
    /// The launcher or the person moved back to the previous version. Shown once.
    RolledBack {
        /// The version that was left.
        from: Version,
        /// Why.
        reason: RollbackReason,
    },
}

/// The model catalogue's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CatalogueStatus {
    /// Never refreshed this session.
    Idle,
    /// Refreshed and verified.
    Fresh {
        /// When.
        at: Timestamp,
        /// How many models it lists.
        entries: u32,
    },
    /// The last refresh failed.
    Failed {
        /// When.
        at: Timestamp,
        /// Why.
        reason: CheckFailure,
    },
}
