//! Names, versions and digests: what a slot, a model and a runtime are called, and the reference that pins a model to
//! its exact bytes.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

omuzaki_core::slug_id!(
    /// A slot's id: `asr`, `vad`, `tts`, `face_tracker`. The crate that owns the slot's data types registers it.
    SlotId
);

omuzaki_core::slug_id!(
    /// A model's id: `silero-vad`, `whisper-base-en`. User models always start with `user-`, so they can never shadow
    /// a shipped one.
    ModelId
);

omuzaki_core::slug_id!(
    /// A runtime's id: `onnx` (ONNX Runtime through `ort`), `whisper-ggml` (whisper.cpp). The crate that implements the
    /// runtime registers it.
    RuntimeId
);

/// Why a string isn't a [`Version`], [`SlotRef`] or [`Sha256`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{what} {got:?}: {rule}")]
pub struct ParseError {
    /// What was being parsed.
    pub what: &'static str,
    /// The text that was given.
    pub got: String,
    /// The rule it breaks.
    pub rule: &'static str,
}

/// A `major.minor.patch` version: a model's own version and the app's (for `min_app_version`). Ordered numerically.
///
/// Only the three numbers of semver: no pre-release or build suffix, because a pinned model has exactly one version
/// and a suffix would only make two names for it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    /// Incompatible change.
    pub major: u32,
    /// Compatible addition.
    pub minor: u32,
    /// Fix.
    pub patch: u32,
}

impl Version {
    /// `major.minor.patch`.
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A decimal number with no sign and no leading zero (`0` itself is fine), as semver writes them.
fn number(part: &str) -> Option<u32> {
    let plain = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && (part == "0" || !part.starts_with('0'));
    if plain { part.parse().ok() } else { None }
}

impl FromStr for Version {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ParseError { what: "version", got: s.to_owned(), rule: "three numbers like 1.4.0" };
        let mut parts = s.split('.').map(number);
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => Ok(Self { major, minor, patch }),
            _ => Err(bad()),
        }
    }
}

impl TryFrom<String> for Version {
    type Error = ParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Version> for String {
    fn from(v: Version) -> Self {
        v.to_string()
    }
}

impl JsonSchema for Version {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Version".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$",
            "description": "A version: major.minor.patch, like 1.4.0."
        })
    }
}

/// A slot at one interface version: `vad@1`. The version is the slot's, not a model's: it changes only when the slot's
/// inputs or outputs change, and a model declares the one it was built for.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SlotRef {
    /// The slot.
    pub slot: SlotId,
    /// Its interface version, from 1.
    pub version: u32,
}

impl SlotRef {
    /// `slot@version`.
    pub fn new(slot: SlotId, version: u32) -> Self {
        Self { slot, version }
    }
}

impl fmt::Display for SlotRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.slot, self.version)
    }
}

impl FromStr for SlotRef {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ParseError { what: "slot", got: s.to_owned(), rule: "a slot name and its version, like vad@1" };
        let (slot, version) = s.split_once('@').ok_or_else(bad)?;
        let slot = SlotId::new(slot).map_err(|_| bad())?;
        match number(version) {
            Some(version) if version > 0 => Ok(Self { slot, version }),
            _ => Err(bad()),
        }
    }
}

impl TryFrom<String> for SlotRef {
    type Error = ParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<SlotRef> for String {
    fn from(s: SlotRef) -> Self {
        s.to_string()
    }
}

impl JsonSchema for SlotRef {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "SlotRef".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[a-z0-9][a-z0-9_-]{0,63}@[1-9][0-9]*$",
            "description": "A slot and its interface version, like vad@1 or face_tracker@1."
        })
    }
}

/// A SHA-256 digest, written as 64 lowercase hex characters.
///
/// Every model file, runtime library and test fixture is pinned by SHA-256 (not the asset store's BLAKE3), because
/// that is what upstreams publish (Hugging Face LFS, release checksums), so a pin can be checked against its source.
/// Computing digests is the implementation's job; this type only names them.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256(pub [u8; 32]);

impl Sha256 {
    /// Parses 64 lowercase hex characters; `None` for anything else. A `const fn`, so catalogue statics are checked
    /// at compile time: `const PIN: Sha256 = match Sha256::from_hex("…") { Some(h) => h, None => panic!() };`.
    pub const fn from_hex(s: &str) -> Option<Self> {
        const fn nibble(c: u8) -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            }
        }
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            match (nibble(bytes[2 * i]), nibble(bytes[2 * i + 1])) {
                (Some(hi), Some(lo)) => out[i] = (hi << 4) | lo,
                _ => return None,
            }
            i += 1;
        }
        Some(Self(out))
    }
}

impl FromStr for Sha256 {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s).ok_or_else(|| ParseError {
            what: "SHA-256",
            got: s.to_owned(),
            rule: "64 lowercase hex characters",
        })
    }
}

impl TryFrom<String> for Sha256 {
    type Error = ParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Sha256> for String {
    fn from(h: Sha256) -> Self {
        h.to_string()
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256({})", &self.to_string()[..12])
    }
}

impl JsonSchema for Sha256 {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Sha256".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{64}$",
            "description": "A SHA-256 digest: 64 lowercase hex characters."
        })
    }
}

/// One model, exactly: its id and version for people, and the digest of its files for machines. This is what
/// selections, sessions, takes and licence records store, so "made with model X" always names the bytes that ran.
///
/// The digest is [`ModelManifest::digest_text`](crate::ModelManifest::digest_text) hashed with SHA-256, so it covers
/// every file of a multi-file model.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct ModelRef {
    /// The model's id.
    pub id: ModelId,
    /// The model's version.
    pub version: Version,
    /// SHA-256 of the model's file list (see above).
    pub digest: Sha256,
}

impl fmt::Display for ModelRef {
    /// `id@version` and the first 12 digest characters: enough for messages and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{} ({})", self.id, self.version, &self.digest.to_string()[..12])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_strictly_and_order_numerically() {
        assert_eq!("1.10.0".parse::<Version>().unwrap(), Version::new(1, 10, 0));
        for bad in ["1.2", "1.2.3.4", "01.2.3", "1.2.3-rc1", "v1.2.3", "", "1..3", "+1.2.3"] {
            assert!(bad.parse::<Version>().is_err(), "{bad}");
        }
        assert!(Version::new(1, 10, 0) > Version::new(1, 9, 9));
        assert_eq!(serde_json::to_string(&Version::new(6, 2, 3)).unwrap(), "\"6.2.3\"");
    }

    #[test]
    fn slot_refs_need_a_slug_and_a_positive_version() {
        let s: SlotRef = "face_tracker@2".parse().unwrap();
        assert_eq!((s.slot.as_str(), s.version), ("face_tracker", 2));
        assert_eq!(s.to_string(), "face_tracker@2");
        for bad in ["vad", "vad@0", "vad@", "@1", "Vad@1", "vad@01", "vad@1@2"] {
            assert!(bad.parse::<SlotRef>().is_err(), "{bad}");
        }
    }

    #[test]
    fn sha256_hex_round_trips_and_is_checked_at_compile_time() {
        const ZERO: Sha256 = match Sha256::from_hex("0000000000000000000000000000000000000000000000000000000000000000")
        {
            Some(h) => h,
            None => panic!("bad pin"),
        };
        assert_eq!(ZERO, Sha256([0; 32]));
        let h = Sha256([0xab; 32]);
        assert_eq!(h.to_string().parse::<Sha256>().unwrap(), h);
        assert!("AB".repeat(32).parse::<Sha256>().is_err(), "uppercase is refused");
        assert!("ab".repeat(31).parse::<Sha256>().is_err());
        assert_eq!(format!("{h:?}"), "Sha256(abababababab)");
    }
}
