//! The [`Signed`] envelope: a body, who signed it and the signature, which is never inside the hash it signs.

use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::canonical::{CanonicalError, canonical_hash, to_json_value};
use crate::domain::Domain;
use crate::key::{Identity, Signature};
use crate::service::{Check, Mismatch, TrustStore};
use crate::verify::{VerifyError, verify};

/// A body with its signer and signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Signed<T> {
    /// What is signed.
    pub body: T,
    /// Who signed it.
    pub signer: Identity,
    /// The signature over [`hash`](Self::hash) under a [`Domain`].
    pub signature: Signature,
}

impl<T: Serialize> Signed<T> {
    /// BLAKE3 of the canonical `{"body": ..., "signer": {"key": ..., "name": ...}}`: the signer's name and key are
    /// bound too, and the signature is not.
    ///
    /// # Errors
    /// [`CanonicalError`] when the body has no JSON form. A signer gets this before anything is signed.
    pub fn hash(&self) -> Result<ContentHash, CanonicalError> {
        hash_of(&self.body, &self.signer)
    }

    /// Verifies the signature under `domain` and looks the key up in `trust`. A hash failure is `Malformed`.
    pub fn check(&self, domain: Domain, trust: &dyn TrustStore) -> Check {
        let hash = match self.hash() {
            Ok(hash) => hash,
            Err(e) => return Check::Malformed(e.to_string()),
        };
        match verify(domain, &hash, &self.signer.key, &self.signature) {
            Ok(()) => Check::Valid { identity: self.signer.clone(), trust: trust.trust(&self.signer.key) },
            Err(VerifyError::BadKey) => Check::Malformed(VerifyError::BadKey.to_string()),
            Err(VerifyError::Mismatch) => Check::Tampered(Mismatch::Signature),
        }
    }
}

/// The hash a signer signs for `body` as `signer`; the same as [`Signed::hash`] before the signature exists.
///
/// # Errors
/// [`CanonicalError`] when the body has no JSON form.
pub fn hash_of<T: Serialize>(body: &T, signer: &Identity) -> Result<ContentHash, CanonicalError> {
    let body = to_json_value(body)?;
    canonical_hash(&json!({ "body": body, "signer": { "key": signer.key, "name": signer.name } }))
}
