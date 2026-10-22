//! The in-memory secret store: the one fake every caller of `omuzaki-secrets-api` tests against (CN-05), and the
//! [`conformance`] suite every real store must pass too.
//!
//! - [`MemorySecrets`] keeps secrets in a map. Nothing touches the OS keychain or the disk, so it works on a CI
//!   machine with no Secret Service daemon.
//! - [`MemorySecrets::fail_with`] makes every call fail with a chosen error, to test a caller's handling of a missing
//!   or locked keychain.
//! - [`conformance::run`] is the behaviour a store must have: set, get, replace, delete, labels and a missing label.
#![deny(missing_docs)]

pub mod conformance;
mod memory;

pub use memory::MemorySecrets;
