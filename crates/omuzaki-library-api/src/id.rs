//! Item ids and versions (ADR 0019, design D2).

use std::fmt;
use std::str::FromStr;

use omuzaki_core::id::{Slug, SlugError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The first segment of every first-party id (`omuzaki/hair/twintails`). Only the Omuzaki publisher key may sign it.
pub const FIRST_PARTY_PUBLISHER: &str = "omuzaki";
/// The first segment reserved for fixtures. Compiled-in test sources use it and nothing installed may.
pub const FIXTURE_PUBLISHER: &str = "test";

/// Why a string isn't an [`ItemId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// Fewer than 2 or more than 4 segments.
    #[error("an item id has 2 to 4 segments joined by '/' (publisher/name), like omuzaki/hair/twintails; got {0}")]
    Segments(usize),
    /// The whole id is longer than [`ItemId::MAX_LEN`].
    #[error("an item id can be at most {max} characters (got {0})", max = ItemId::MAX_LEN)]
    TooLong(usize),
    /// A segment isn't a valid slug.
    #[error("segment {index} ({segment:?}) is not a valid name: {source}")]
    Segment {
        /// Which segment, from 0.
        index: usize,
        /// The segment as written.
        segment: String,
        /// Why it was refused.
        source: SlugError,
    },
}

/// An item id, `publisher/name`: two to four slug segments joined by `/`, at most 96 characters.
///
/// The first segment is the publisher's handle. `omuzaki/` ids belong to the Omuzaki publisher key and `test/` to
/// fixtures; whether an id is *valid under its key* is decided by `ItemManifest::verify`, not here, because the
/// id alone says nothing about who signed it.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct ItemId(String);

impl ItemId {
    /// The longest allowed id, in characters.
    pub const MAX_LEN: usize = 96;

    /// Validates `s` as an item id.
    ///
    /// # Errors
    /// An [`IdError`] naming the first rule `s` breaks.
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        let segments = s.split('/').count();
        if !(2..=4).contains(&segments) {
            return Err(IdError::Segments(segments));
        }
        let len = s.chars().count();
        if len > Self::MAX_LEN {
            return Err(IdError::TooLong(len));
        }
        for (index, segment) in s.split('/').enumerate() {
            Slug::new(segment).map_err(|source| IdError::Segment { index, segment: segment.to_owned(), source })?;
        }
        Ok(Self(s))
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The publisher segment (`omuzaki` in `omuzaki/hair/twintails`).
    pub fn publisher(&self) -> &str {
        self.0.split('/').next().unwrap_or_default()
    }

    /// The segments after the publisher, joined (`hair/twintails`).
    pub fn name(&self) -> &str {
        self.0.split_once('/').map_or("", |(_, name)| name)
    }

    /// The last segment (`twintails`), which names the file of a first-party item and the application key of an
    /// item without a slot.
    pub fn last_segment(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or_default()
    }

    /// True for ids under the Omuzaki publisher (`omuzaki/...`).
    pub fn is_first_party(&self) -> bool {
        self.publisher() == FIRST_PARTY_PUBLISHER
    }

    /// True for ids under the fixture publisher (`test/...`).
    pub fn is_fixture(&self) -> bool {
        self.publisher() == FIXTURE_PUBLISHER
    }
}

impl TryFrom<String> for ItemId {
    type Error = IdError;
    fn try_from(s: String) -> Result<Self, IdError> {
        Self::new(s)
    }
}

impl From<ItemId> for String {
    fn from(id: ItemId) -> Self {
        id.0
    }
}

impl FromStr for ItemId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        Self::new(s)
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ItemId({})", self.0)
    }
}

/// Why a string isn't a [`Version`] or a [`VersionReq`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    /// Not `MAJOR.MINOR.PATCH` of plain numbers (no pre-release or build parts, no leading zeros).
    #[error("{0:?} is not a version: write MAJOR.MINOR.PATCH, like 1.2.0")]
    Version(String),
    /// Not `=x.y.z`, `^x.y` or `*`.
    #[error("{0:?} is not a version range: write =1.2.0, ^1.2 (same major, at least 1.2) or *")]
    Req(String),
}

/// A plain semantic version, `MAJOR.MINOR.PATCH`. What a bump means for an item is in design D16.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct Version {
    /// Incompatible changes.
    pub major: u32,
    /// Compatible additions.
    pub minor: u32,
    /// Value-only fixes.
    pub patch: u32,
}

/// One numeric component: digits only, no leading zero (`0` is fine).
fn component(s: &str) -> Option<u32> {
    let plain = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    if plain { s.parse().ok() } else { None }
}

impl Version {
    /// A version from its parts.
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    /// Parses `MAJOR.MINOR.PATCH`.
    ///
    /// # Errors
    /// [`VersionError::Version`] for anything else.
    pub fn parse(s: &str) -> Result<Self, VersionError> {
        let bad = || VersionError::Version(s.to_owned());
        let mut parts = s.split('.');
        let (Some(a), Some(b), Some(c), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(bad());
        };
        match (component(a), component(b), component(c)) {
            (Some(major), Some(minor), Some(patch)) => Ok(Self { major, minor, patch }),
            _ => Err(bad()),
        }
    }
}

impl TryFrom<String> for Version {
    type Error = VersionError;
    fn try_from(s: String) -> Result<Self, VersionError> {
        Self::parse(&s)
    }
}

impl From<Version> for String {
    fn from(v: Version) -> Self {
        v.to_string()
    }
}

impl FromStr for Version {
    type Err = VersionError;
    fn from_str(s: &str) -> Result<Self, VersionError> {
        Self::parse(s)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Version({self})")
    }
}

/// Which versions a pin or a bundle's item step accepts.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VersionReq {
    /// `*`: any version.
    Any,
    /// `=x.y.z`: exactly that version.
    Exact(Version),
    /// `^x`, `^x.y` or `^x.y.z`: the same major and at least the given version (missing parts are 0). Items keep
    /// breaking changes to major bumps (design D16), so this is the "compatible update" range.
    ///
    /// Unlike Cargo, major zero is not special: the major is always the boundary, so `^0.2` matches `0.3.0`.
    Compatible(Version),
}

impl VersionReq {
    /// Parses `=x.y.z`, `^x`, `^x.y`, `^x.y.z` or `*`.
    ///
    /// # Errors
    /// [`VersionError::Req`] for anything else.
    pub fn parse(s: &str) -> Result<Self, VersionError> {
        let bad = || VersionError::Req(s.to_owned());
        if s == "*" {
            return Ok(Self::Any);
        }
        if let Some(exact) = s.strip_prefix('=') {
            return Version::parse(exact).map(Self::Exact).map_err(|_| bad());
        }
        let caret = s.strip_prefix('^').ok_or_else(bad)?;
        let parts: Vec<Option<u32>> = caret.split('.').map(component).collect();
        match parts.as_slice() {
            [Some(a)] => Ok(Self::Compatible(Version::new(*a, 0, 0))),
            [Some(a), Some(b)] => Ok(Self::Compatible(Version::new(*a, *b, 0))),
            [Some(a), Some(b), Some(c)] => Ok(Self::Compatible(Version::new(*a, *b, *c))),
            _ => Err(bad()),
        }
    }

    /// True if `v` is accepted.
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(want) => v == want,
            Self::Compatible(min) => v.major == min.major && v >= min,
        }
    }
}

impl TryFrom<String> for VersionReq {
    type Error = VersionError;
    fn try_from(s: String) -> Result<Self, VersionError> {
        Self::parse(&s)
    }
}

impl From<VersionReq> for String {
    fn from(r: VersionReq) -> Self {
        r.to_string()
    }
}

impl FromStr for VersionReq {
    type Err = VersionError;
    fn from_str(s: &str) -> Result<Self, VersionError> {
        Self::parse(s)
    }
}

impl fmt::Display for VersionReq {
    /// The normalised form: `*`, `=1.2.0` or `^1.2.0`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("*"),
            Self::Exact(v) => write!(f, "={v}"),
            Self::Compatible(v) => write!(f, "^{v}"),
        }
    }
}

impl Serialize for VersionReq {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for VersionReq {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for VersionReq {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VersionReq".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": r"^(\*|=\d+\.\d+\.\d+|\^\d+(\.\d+(\.\d+)?)?)$",
            "description": "A version range: =1.2.0 (exactly), ^1.2 (same major, at least 1.2) or * (any)."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_two_to_four_slug_segments() {
        for ok in ["omuzaki/hair", "omuzaki/hair/twintails", "a/b/c/d", "test/x"] {
            assert_eq!(ItemId::new(ok).unwrap().as_str(), ok);
        }
        assert_eq!(ItemId::new("omuzaki"), Err(IdError::Segments(1)));
        assert_eq!(ItemId::new("a/b/c/d/e"), Err(IdError::Segments(5)));
        assert!(matches!(ItemId::new("Omuzaki/hair"), Err(IdError::Segment { index: 0, .. })));
        assert!(matches!(ItemId::new("omuzaki//hair"), Err(IdError::Segment { index: 1, .. })));
        assert!(matches!(ItemId::new("omuzaki/hair twin"), Err(IdError::Segment { index: 1, .. })));
        let long = format!("a/{}/{}", "b".repeat(50), "c".repeat(50));
        assert_eq!(ItemId::new(long), Err(IdError::TooLong(103)));
    }

    #[test]
    fn id_parts_and_namespaces() {
        let id = ItemId::new("omuzaki/hair/twintails").unwrap();
        assert_eq!((id.publisher(), id.name(), id.last_segment()), ("omuzaki", "hair/twintails", "twintails"));
        assert!(id.is_first_party() && !id.is_fixture());
        assert!(ItemId::new("test/fixture").unwrap().is_fixture());
        assert!(!ItemId::new("mira/hair").unwrap().is_first_party());
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"omuzaki/hair/twintails\"");
        assert!(serde_json::from_str::<ItemId>("\"nope\"").is_err());
    }

    #[test]
    fn versions_are_plain_semver() {
        assert_eq!(Version::parse("1.20.3").unwrap(), Version::new(1, 20, 3));
        for bad in ["1.2", "1.2.3.4", "01.2.3", "1.2.3-beta", "1.2.3+b", "v1.2.3", "1..3", "", "-1.0.0"] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
        assert!(Version::new(1, 10, 0) > Version::new(1, 9, 9), "numeric, not textual, order");
        assert_eq!(Version::new(2, 0, 1).to_string(), "2.0.1");
    }

    #[test]
    fn ranges_match_as_documented() {
        let v = |s: &str| Version::parse(s).unwrap();
        let req = |s: &str| VersionReq::parse(s).unwrap();
        assert!(req("*").matches(&v("9.9.9")));
        assert!(req("=1.2.0").matches(&v("1.2.0")) && !req("=1.2.0").matches(&v("1.2.1")));
        assert!(req("^1.2").matches(&v("1.2.0")) && req("^1.2").matches(&v("1.9.4")));
        assert!(!req("^1.2").matches(&v("1.1.9")) && !req("^1.2").matches(&v("2.0.0")));
        assert!(req("^1").matches(&v("1.0.0")) && !req("^1").matches(&v("0.9.0")));
        assert!(req("^0.2").matches(&v("0.3.0")), "the major is the compatibility boundary, as in design D16");
        for bad in ["", "1.2.0", "^", "^1.", "^a", "=1.2", "~1.2", "^1.2.3.4", ">=1.0.0", "**"] {
            assert!(VersionReq::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(req("^1").to_string(), "^1.0.0");
        assert_eq!(serde_json::from_str::<VersionReq>("\"^1.2\"").unwrap(), req("^1.2"));
        assert_eq!(serde_json::to_string(&req("=1.2.0")).unwrap(), "\"=1.2.0\"");
    }
}
