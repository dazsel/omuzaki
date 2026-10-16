//! The secret-store contract (CN-05; design in `docs/design/automation.md` D11): where API keys and tokens live.
//!
//! Secrets belong in the operating system's keychain, never in a project file, a log, an event, an op record or MCP
//! output. This crate makes the leak hard to write by accident:
//!
//! - [`label::SecretLabel`] names a secret (`automation/streamerbot`). A label is not secret, so it can be stored, shown and
//!   logged.
//! - [`value::SecretValue`] is the secret itself. It is zeroed on drop and has no `Debug`, `Display`, `Serialize` or `Clone`;
//!   the only way to read it is [`value::SecretValue::expose`].
//! - [`store::SecretStore`] is the one trait that stores them. The real store is `omuzaki-secrets`, the fake
//!   `omuzaki-secrets-mock`.
//! - [`error::SecretError`] never contains a value.
//!
//! **No op reads or writes a secret value.** An op's arguments and outputs are logged, so a value never passes through
//! one. The app wires [`store::SecretStore::set`] and [`store::SecretStore::delete`] to the approval card; an op such as `net.http`
//! resolves labels inside its own run.
#![deny(missing_docs)]

pub mod error;
pub mod label;
pub mod store;
pub mod value;

/// The few items almost every caller names: the label, the value, the store and its error.
pub mod prelude {
    pub use crate::error::SecretError;
    pub use crate::label::SecretLabel;
    pub use crate::store::SecretStore;
    pub use crate::value::SecretValue;
}
