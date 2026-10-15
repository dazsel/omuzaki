//! What a check finds.

use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};

/// An https URL. Used for release-notes pages only, which the person may open in a browser.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Url(pub String);

/// An update channel. It is part of the feed path, never of the request.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Released versions.
    Stable,
    /// Earlier builds, opt-in.
    Beta,
}

/// How a release reaches this install.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PackageKind {
    /// Per-file patches against an installed version.
    Delta {
        /// The version the patches apply to.
        from: Version,
    },
    /// A whole app tree.
    Full,
}

/// A newer version the signed manifest offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Release {
    /// The version offered.
    pub version: Version,
    /// The channel it came from.
    pub channel: Channel,
    /// The manifest's monotonic sequence.
    pub sequence: u64,
    /// Delta or full.
    pub package: PackageKind,
    /// Bytes to download.
    pub download_bytes: u64,
    /// Bytes on disk once installed.
    pub installed_bytes: u64,
    /// An optional page of release notes.
    pub notes_url: Option<Url>,
    /// True when the running version is below the manifest's `yanked_below`: a stronger "update recommended".
    pub recommended: bool,
}
