//! What a check finds.

use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::str::FromStr;
use thiserror::Error;

/// An https URL. Used for release-notes pages only.
///
/// The only way in is [`Url::new`], and deserialization goes through it too, so a `Url` value is always: an `https://`
/// URL (the scheme in any case) with a non-empty ASCII host made of letters, digits, `-` and `.` (no leading or trailing
/// dot), an optional numeric port from 1 to 65535, no user information, and no spaces, control or non-ASCII characters.
/// A host made only of digits and dots must be a valid dotted-quad IPv4 address (`std::net::Ipv4Addr`), so `256.0.0.1`
/// and `9999999999999` are rejected; `127.0.0.1` is accepted, because the contract has no network policy. A manifest
/// that offers `http:`, `file:` or a non-URL fails to parse as a whole.
///
/// This is a syntax check and nothing more. It does not guarantee that the host resolves, that the page exists, or that
/// a browser would open the text as written (hex and octal host forms, IDN and the like are not modelled). The app
/// opens the URL through the operating system's handler, which does its own parsing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct Url(String);

/// Why a string is not an https URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Error)]
pub enum UrlError {
    /// The scheme is not `https`.
    #[error("not an https URL")]
    NotHttps,
    /// The scheme is https, but the rest is not a URL with a host: spaces or other non-ASCII characters, a missing
    /// host, a bad or out-of-range port, an invalid numeric host, or user information (`user@host`).
    #[error("malformed https URL")]
    Malformed,
}

impl Url {
    /// Parses `text` as an https URL.
    ///
    /// # Errors
    /// [`UrlError::NotHttps`] when the scheme is not `https` (any case); [`UrlError::Malformed`] when the rest is not
    /// a URL with a host.
    pub fn new(text: impl Into<String>) -> Result<Self, UrlError> {
        let text = text.into();
        let rest = match text.get(..8) {
            Some(scheme) if scheme.eq_ignore_ascii_case("https://") => &text[8..],
            _ => return Err(UrlError::NotHttps),
        };
        if !rest.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(UrlError::Malformed);
        }
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if !valid_authority(authority) {
            return Err(UrlError::Malformed);
        }
        Ok(Self(text))
    }

    /// The URL as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Url {
    type Error = UrlError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(text)
    }
}

impl From<Url> for String {
    fn from(url: Url) -> Self {
        url.0
    }
}

/// `host` or `host:port`, with no user information. The host is letters, digits, `-` and `.`, not empty, no leading or
/// trailing dot, and an all-digits-and-dots host must be a valid dotted-quad IPv4 address. The port, if present, is
/// 1 to 65535.
fn valid_authority(authority: &str) -> bool {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let port_ok = port.is_none_or(|p| {
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n != 0)
    });
    let host_ok = !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    let numeric = host.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    port_ok && host_ok && (!numeric || Ipv4Addr::from_str(host).is_ok())
}

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
