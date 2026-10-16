//! The OS keychain secret store (CN-05): [`KeychainSecrets`] implements `omuzaki_secrets_api::store::SecretStore` over
//! Windows Credential Manager, the macOS Keychain and the Linux Secret Service, through the `keyring` crate.
//!
//! - One keychain **service name**, [`SERVICE`] (`omuzaki`); one entry per label, with the label as the entry's user
//!   name.
//! - The keychain APIs cannot list entries in one portable way, so Omuzaki keeps its own labels in a few extra entries
//!   (the *label index*, labels only, never values). [`labels`](omuzaki_secrets_api::store::SecretStore::labels)
//!   therefore lists Omuzaki's entries and never another program's. The index is split so no entry is over the
//!   smallest keychain limit (Windows: 2,560 bytes, counted in UTF-16), and a change is published as a whole new
//!   generation of shards before one head record switches to it, so a failed write leaves the old list readable.
//! - One writer at a time: storing, deleting and listing hold an exclusive lock on [`LOCK_FILE`] in the data
//!   directory given to [`KeychainSecrets::new`], across every handle and every Omuzaki process that uses it.
//! - A machine with no keychain (Linux without a Secret Service daemon) is
//!   [`SecretError::Unavailable`](omuzaki_secrets_api::error::SecretError::Unavailable) with a hint. There is **no file
//!   fallback**: a secret is in the keychain or it is not stored.
//! - Tests run the store through `omuzaki-secrets-mock`'s conformance suite. The tests that need a real keychain are
//!   `#[ignore]`d because CI on Linux has no Secret Service daemon; the owner runs them (see `tests/keychain.rs`).
#![deny(missing_docs)]

mod error;
mod index;
mod store;

pub use store::{KeychainSecrets, LOCK_FILE, SERVICE};
