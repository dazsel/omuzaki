//! [`SecretValue`]: the secret itself.

use zeroize::Zeroizing;

use crate::error::SecretError;

/// A secret's value, zeroed when dropped.
///
/// It has no `Debug`, `Display`, `Serialize` or `Clone`, so it cannot reach a log, an event, an op record or MCP
/// output by accident. The one way to read it is [`expose`](Self::expose), which the caller uses to build the one
/// request header (or the like) that needs it and nothing else.
///
/// ```compile_fail,E0277
/// fn needs_debug<T: std::fmt::Debug>() {}
/// needs_debug::<omuzaki_secrets_api::value::SecretValue>();
/// ```
///
/// ```compile_fail,E0277
/// fn needs_display<T: std::fmt::Display>() {}
/// needs_display::<omuzaki_secrets_api::value::SecretValue>();
/// ```
///
/// ```compile_fail,E0277
/// fn needs_serialize<T: serde::Serialize>() {}
/// needs_serialize::<omuzaki_secrets_api::value::SecretValue>();
/// ```
///
/// ```compile_fail,E0277
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<omuzaki_secrets_api::value::SecretValue>();
/// ```
pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    /// Wraps `value`. The text is moved in, not copied; the caller's other copies are the caller's to zero.
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// The value, for the one use that needs it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the store can keep this value: every store refuses an empty one, because some platforms cannot hold
    /// it and a store must not behave differently from the next. Stores call this at the top of `set`.
    ///
    /// # Errors
    /// [`SecretError::Rejected`] for an empty value.
    pub fn check_storable(&self) -> Result<(), SecretError> {
        if self.0.is_empty() {
            return Err(SecretError::rejected("a secret cannot be empty; delete it instead"));
        }
        Ok(())
    }
}
