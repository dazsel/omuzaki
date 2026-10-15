//! [`SecretStore`]: the one trait that keeps secrets.

use crate::{SecretError, SecretLabel, SecretValue};

/// A place that keeps secrets by label: the OS keychain in the app (`omuzaki-secrets`), a map in tests
/// (`omuzaki-secrets-mock`).
///
/// A store never writes a secret to a file, a log or an error, and never falls back to one when its keychain is
/// missing: it reports [`SecretError::Unavailable`] instead. Every store passes the conformance suite in
/// `omuzaki-secrets-mock`.
pub trait SecretStore: Send + Sync {
    /// The secret stored under `label`, or `None` when there is none.
    ///
    /// # Errors
    /// [`SecretError`] when the keychain is missing, refuses access or holds unreadable data.
    fn get(&self, label: &SecretLabel) -> Result<Option<SecretValue>, SecretError>;

    /// Whether a secret is stored under `label`.
    ///
    /// # Errors
    /// As [`get`](Self::get).
    fn contains(&self, label: &SecretLabel) -> Result<bool, SecretError>;

    /// The labels Omuzaki stored, in order (not other programs' keychain entries).
    ///
    /// # Errors
    /// As [`get`](Self::get).
    fn labels(&self) -> Result<Vec<SecretLabel>, SecretError>;

    /// Stores `value` under `label`, replacing any earlier one. An empty value is
    /// [`SecretError::Rejected`] (see [`SecretValue::check_storable`]).
    ///
    /// # Errors
    /// As [`get`](Self::get), and [`SecretError::Rejected`].
    fn set(&self, label: &SecretLabel, value: SecretValue) -> Result<(), SecretError>;

    /// Removes the secret under `label`. A label with no secret is not an error: the postcondition holds.
    ///
    /// # Errors
    /// As [`get`](Self::get).
    fn delete(&self, label: &SecretLabel) -> Result<(), SecretError>;
}
