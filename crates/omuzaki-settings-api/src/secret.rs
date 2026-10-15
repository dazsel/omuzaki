//! A reference to a secret (D1): a label, never the secret.

use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::error::{SettingError, SettingErrorKind};

/// A label that names a secret in the OS keychain (CN-05), such as `connector/openai`.
///
/// There is **no accessor for the value** and none can be added here: the keychain wrapper takes a `SecretRef` and
/// returns the secret to the one caller that needs it. A setting that holds a `SecretRef` can therefore be logged,
/// put in an event, shown to an agent or written to `settings.toml` without a leak. Its schema is a plain string:
/// `format: "secret"` is forbidden in a setting's schema (the [`check`](crate::check) test fails it), because a secret
/// must not be a setting value at all.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SecretRef(String);

impl SecretRef {
    /// The longest label.
    pub const MAX_LEN: usize = 128;

    /// Validates `label`: 1 to [`MAX_LEN`](Self::MAX_LEN) characters of `a-z`, `0-9`, `-`, `_`, `.` and `/`, with no
    /// empty segment and no `..`. The shape keeps a label from being mistaken for a key, since keys never contain `/`
    /// and secrets never contain spaces.
    ///
    /// # Errors
    /// [`SettingErrorKind::BadSecretRef`] (the key is empty: the caller knows which setting it was for).
    pub fn new(label: impl Into<String>) -> Result<Self, SettingError> {
        let label = label.into();
        let bad = |reason: &str| SettingError::new("", SettingErrorKind::BadSecretRef { reason: reason.into() });
        if label.is_empty() || label.len() > Self::MAX_LEN {
            return Err(bad(&format!("a label is 1 to {} characters", Self::MAX_LEN)));
        }
        if !label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_./".contains(c)) {
            return Err(bad("use a-z, 0-9, '-', '_', '.' and '/'"));
        }
        if label.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
            return Err(bad("no empty segment, '.' or '..'"));
        }
        Ok(Self(label))
    }

    /// The label (not the secret).
    #[must_use]
    pub fn label(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SecretRef {
    type Error = SettingError;
    fn try_from(label: String) -> Result<Self, Self::Error> {
        Self::new(label)
    }
}

impl From<SecretRef> for String {
    fn from(r: SecretRef) -> Self {
        r.0
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretRef({})", self.0)
    }
}

impl JsonSchema for SecretRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SecretRef".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "The label of a secret held in the OS keychain, never the secret.",
            "maxLength": Self::MAX_LEN,
            "pattern": "^[a-z0-9._-]+(/[a-z0-9._-]+)*$"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_labels_and_rejects_the_rest() {
        assert_eq!(SecretRef::new("connector/openai").unwrap().label(), "connector/openai");
        for bad in ["", "A", "a b", "/a", "a//b", "a/../b", "sk-Abc"] {
            assert!(SecretRef::new(bad).is_err(), "{bad}");
        }
        assert!(SecretRef::new("x".repeat(SecretRef::MAX_LEN + 1)).is_err());
    }

    #[test]
    fn serde_validates_and_round_trips() {
        let r = SecretRef::new("stream/twitch").unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"stream/twitch\"");
        assert_eq!(serde_json::from_str::<SecretRef>("\"stream/twitch\"").unwrap(), r);
        assert!(serde_json::from_str::<SecretRef>("\"Not A Label\"").is_err());
    }
}
