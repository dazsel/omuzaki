//! The services and outcomes: [`Signer`], [`TrustStore`] and what verifying reports as [`Check`].

use omuzaki_store_api::ContentHash;

use crate::domain::Domain;
use crate::key::{Identity, KeyId, Signature};

/// Why signing failed. Never contains key bytes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SignError {
    /// No key can be reached (for example no keychain), with a hint for the person.
    #[error("signing is unavailable: {0}")]
    Unavailable(String),
    /// The signer failed for another reason.
    #[error("signing failed: {0}")]
    Failed(String),
}

/// A signing service. The private key never leaves it. Callers that sign on a person's behalf must hold a person's
/// approval first (design D8).
pub trait Signer: Send + Sync {
    /// The name and public key this signer signs as.
    ///
    /// # Errors
    /// [`SignError`] when no key can be reached.
    fn identity(&self) -> Result<Identity, SignError>;

    /// Signs the `domain` message for `hash`. ed25519 is deterministic: the same input gives the same bytes.
    ///
    /// # Errors
    /// [`SignError`] when signing is unavailable or fails.
    fn sign(&self, domain: Domain, hash: &ContentHash) -> Result<Signature, SignError>;
}

/// What a person's trust list says about a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// On the list; `name` is the person's own label for the key, not the signer's claim.
    Trusted {
        /// The person's label.
        name: String,
    },
    /// Not on the list.
    Unknown,
    /// Marked revoked: every signature by this key.
    Revoked,
}

/// A person's list of pinned keys. Validity is maths; trust is a person's choice.
pub trait TrustStore: Send + Sync {
    /// What the list says about `key`.
    fn trust(&self, key: &KeyId) -> Trust;
}

/// Why a signature did not hold.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// The signed bytes changed.
    Content,
    /// The signature does not hold for the content and key.
    Signature,
}

/// The outcome of checking a signed thing; every surface shows these the same way (design D4).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Check {
    /// The signature holds. `trust` is what the person's list says about the key.
    Valid {
        /// The claimed name and the key that verified.
        identity: Identity,
        /// The person's trust in that key.
        trust: Trust,
    },
    /// Changed after it was signed. The claimed name is not shown as fact.
    Tampered(Mismatch),
    /// Not signed.
    Unsigned,
    /// A broken signature block, with the reason.
    Malformed(String),
}
