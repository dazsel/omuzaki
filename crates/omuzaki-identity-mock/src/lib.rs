//! Test doubles for `omuzaki-identity-api`: a signer made from a fixed seed and an in-memory trust list.
//!
//! Test seeds exist only here, never in a shipped crate (design D1).
#![deny(missing_docs)]

use std::collections::BTreeMap;
use std::fmt;

use ed25519_dalek::{Signer as _, SigningKey};
use omuzaki_identity_api::domain::{Domain, signing_message};
use omuzaki_identity_api::key::{Identity, KeyId, Signature};
use omuzaki_identity_api::service::{SignError, Signer, Trust, TrustStore};
use omuzaki_store_api::ContentHash;

/// A [`Signer`] over a fixed seed. For tests only.
pub struct MemorySigner {
    key: SigningKey,
    name: String,
}

impl MemorySigner {
    /// A signer whose ed25519 seed is `seed`, signing as `name`.
    pub fn from_test_seed(seed: [u8; 32], name: &str) -> Self {
        Self { key: SigningKey::from_bytes(&seed), name: name.to_owned() }
    }

    /// The public key.
    pub fn key_id(&self) -> KeyId {
        KeyId(self.key.verifying_key().to_bytes())
    }
}

impl fmt::Debug for MemorySigner {
    /// The name and fingerprint only, never key bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemorySigner({:?}, {})", self.name, self.key_id().fingerprint())
    }
}

impl Signer for MemorySigner {
    fn identity(&self) -> Result<Identity, SignError> {
        Ok(Identity { name: self.name.clone(), key: self.key_id() })
    }

    fn sign(&self, domain: Domain, hash: &ContentHash) -> Result<Signature, SignError> {
        Ok(Signature(self.key.sign(&signing_message(domain, hash)).to_bytes()))
    }
}

/// A [`TrustStore`] held in memory. A key not added is `Unknown`; revoking wins over trusting.
#[derive(Clone, Debug, Default)]
pub struct MemoryTrust {
    entries: BTreeMap<KeyId, Trust>,
}

impl MemoryTrust {
    /// An empty list: every key is unknown.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trusts `key` under the person's own label `name`, unless it is already revoked.
    #[must_use]
    pub fn trusted(mut self, key: KeyId, name: &str) -> Self {
        self.entries.entry(key).or_insert_with(|| Trust::Trusted { name: name.to_owned() });
        self
    }

    /// Marks `key` revoked.
    #[must_use]
    pub fn revoked(mut self, key: KeyId) -> Self {
        self.entries.insert(key, Trust::Revoked);
        self
    }
}

impl TrustStore for MemoryTrust {
    fn trust(&self, key: &KeyId) -> Trust {
        self.entries.get(key).cloned().unwrap_or(Trust::Unknown)
    }
}
