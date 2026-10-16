//! The OS keychain secret store (CN-05): [`KeychainSecrets`] implements `omuzaki_secrets_api::SecretStore` over
//! Windows Credential Manager, the macOS Keychain and the Linux Secret Service, through the `keyring` crate.
//!
//! - One keychain **service name**, [`SERVICE`] (`omuzaki`); one entry per label, with the label as the entry's user
//!   name.
//! - The keychain APIs cannot list entries in one portable way, so Omuzaki keeps its own labels in a few extra entries
//!   (the *label index*, labels only, never values; split so no entry is over the smallest keychain limit).
//!   [`labels`](omuzaki_secrets_api::SecretStore::labels) therefore lists Omuzaki's entries and never another
//!   program's. Stores in one process share a lock around index changes; another program changing the index at the
//!   same moment is caught by reading it back and making the change again.
//! - A machine with no keychain (Linux without a Secret Service daemon) is
//!   [`SecretError::Unavailable`](omuzaki_secrets_api::SecretError::Unavailable) with a hint. There is **no file
//!   fallback**: a secret is in the keychain or it is not stored.
//! - Tests run the store through `omuzaki-secrets-mock`'s conformance suite. The tests that need a real keychain are
//!   `#[ignore]`d because CI on Linux has no Secret Service daemon; the owner runs them (see `tests/keychain.rs`).
#![deny(missing_docs)]

mod error;
mod index;
mod store;

pub use store::{KeychainSecrets, SERVICE};
