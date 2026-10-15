//! Keys, signatures and identities, all written as lowercase hex (design D3).

use std::fmt::Write as _;

use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why a string isn't a [`KeyId`] or a [`Signature`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum KeyError {
    /// The text isn't exactly the expected number of characters.
    #[error("expected {expected} hex characters, found {found}")]
    Length {
        /// The characters required.
        expected: usize,
        /// The characters given.
        found: usize,
    },
    /// The text has a character that isn't a hex digit.
    #[error("not hexadecimal")]
    NotHex,
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(2 * bytes.len()), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Reads exactly `2 * N` hex digits, either case.
pub(crate) fn from_hex<const N: usize>(s: &str) -> Result<[u8; N], KeyError> {
    if s.len() != 2 * N {
        return Err(KeyError::Length { expected: 2 * N, found: s.len() });
    }
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(KeyError::NotHex);
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| KeyError::NotHex)?;
    }
    Ok(out)
}

/// An ed25519 public key, which is also the key's identifier. Serialised as 64 lowercase hex characters.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct KeyId(pub [u8; 32]);

impl KeyId {
    /// Reads 64 hex characters (either case).
    ///
    /// # Errors
    /// [`KeyError`] when the length is not 64 or a character is not hex.
    pub fn from_hex(s: &str) -> Result<Self, KeyError> {
        from_hex::<32>(s).map(Self)
    }

    /// The key as 64 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// The first 16 hex characters of the BLAKE3 hash of the key: for display only, too short to bind anything.
    pub fn fingerprint(&self) -> String {
        ContentHash::of(&self.0).to_string()[..16].to_owned()
    }

    /// The 8 bytes behind [`fingerprint`](Self::fingerprint), the watermark payload (design D9).
    pub fn mark_payload(&self) -> [u8; 8] {
        let hash = ContentHash::of(&self.0).0;
        [hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7]]
    }
}

impl std::fmt::Debug for KeyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeyId({})", self.fingerprint())
    }
}

impl TryFrom<String> for KeyId {
    type Error = KeyError;
    fn try_from(s: String) -> Result<Self, KeyError> {
        Self::from_hex(&s)
    }
}

impl From<KeyId> for String {
    fn from(key: KeyId) -> Self {
        key.to_hex()
    }
}

/// An ed25519 signature. Serialised as 128 lowercase hex characters.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct Signature(pub [u8; 64]);

impl Signature {
    /// Reads 128 hex characters (either case).
    ///
    /// # Errors
    /// [`KeyError`] when the length is not 128 or a character is not hex.
    pub fn from_hex(s: &str) -> Result<Self, KeyError> {
        from_hex::<64>(s).map(Self)
    }

    /// The signature as 128 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }
}

impl std::fmt::Debug for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Signature({}..)", &self.to_hex()[..8])
    }
}

impl TryFrom<String> for Signature {
    type Error = KeyError;
    fn try_from(s: String) -> Result<Self, KeyError> {
        Self::from_hex(&s)
    }
}

impl From<Signature> for String {
    fn from(sig: Signature) -> Self {
        sig.to_hex()
    }
}

/// Who signed: a name and the key that backs it. A name is never trusted without its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// What the signer calls themself.
    pub name: String,
    /// The public key.
    pub key: KeyId,
}
