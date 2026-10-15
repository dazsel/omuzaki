//! Contract for identity: who signed this (spec v3 SE-07, design `docs/design/identity.md`).
//!
//! One ed25519 key per person, one signature envelope, one trust list and one verifier. This crate is the format and
//! the verifier: pure, deterministic, no randomness, no key storage, and it builds for `wasm32` so the web player
//! verifies with the same code. Private keys live behind [`service::Signer`] in the implementation crate.
//!
//! - [`key`]: [`KeyId`](key::KeyId), [`Signature`](key::Signature) and [`Identity`](key::Identity)
//! - [`domain`]: [`Domain`](domain::Domain), one tag per use, and [`signing_message`](domain::signing_message)
//! - [`verify`]: the strict ed25519 check
//! - [`canonical`]: the canonical JSON form and [`canonical_hash`](canonical::canonical_hash)
//! - [`signed`]: the [`Signed`](signed::Signed) envelope
//! - [`service`]: [`Signer`](service::Signer), [`TrustStore`](service::TrustStore) and the [`Check`](service::Check)
//!   outcomes
#![deny(missing_docs)]

pub mod canonical;
pub mod domain;
pub mod key;
pub mod service;
pub mod signed;
pub mod verify;

/// The names most callers want.
pub mod prelude {
    pub use crate::domain::Domain;
    pub use crate::key::{Identity, KeyId, Signature};
    pub use crate::service::{Check, Signer, Trust, TrustStore};
    pub use crate::signed::Signed;
}
