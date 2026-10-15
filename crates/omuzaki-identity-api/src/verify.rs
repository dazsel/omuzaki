//! The verifier: strict ed25519 over the domain's signing message.

use ed25519_dalek::VerifyingKey;
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::{Domain, signing_message};
use crate::key::{KeyId, Signature};

/// Why a signature did not verify.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VerifyError {
    /// The key is not a valid ed25519 public key.
    #[error("the key is not a valid ed25519 public key")]
    BadKey,
    /// The signature does not hold for this domain, hash and key.
    #[error("the signature does not match this content and key")]
    Mismatch,
}

/// Checks `sig` over the `domain` message for `hash` against `key`, with `verify_strict`: malleable encodings and
/// weak keys are refused, so one content has one valid signature per key.
///
/// # Errors
/// [`VerifyError::BadKey`] for an invalid key, [`VerifyError::Mismatch`] when the signature does not hold.
pub fn verify(domain: Domain, hash: &ContentHash, key: &KeyId, sig: &Signature) -> Result<(), VerifyError> {
    let verifying = VerifyingKey::from_bytes(&key.0).map_err(|_| VerifyError::BadKey)?;
    verifying
        .verify_strict(&signing_message(domain, hash), &ed25519_dalek::Signature::from_bytes(&sig.0))
        .map_err(|_| VerifyError::Mismatch)
}
