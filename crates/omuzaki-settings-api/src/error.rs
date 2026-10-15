//! Errors and findings. Every one names the key, so a person or an agent knows which setting to fix.
//!
//! Localisation (l10n-ops §5): the English [`Display`](std::fmt::Display) text is the fallback that logs and MCP show;
//! a person's surface derives its message id from [`SettingErrorKind::id`] (`setting-error-out-of-range`) and fills the
//! arguments from the variant's fields, which are strings so they cross the wire unchanged.

use std::fmt;

use omuzaki_doc_api::Value;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why a settings call failed, and for which key. `key` is empty only for errors about no key (a profile name).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[error("{}", self.english())]
#[non_exhaustive]
pub struct SettingError {
    /// The setting the call was about, such as `ui.scale`.
    pub key: String,
    /// What went wrong.
    pub kind: SettingErrorKind,
}

/// The kinds of [`SettingError`]. Stable: agents and the UI branch on [`id`](SettingErrorKind::id).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SettingErrorKind {
    /// No registered setting has this key.
    UnknownKey,
    /// The value does not have the setting's type or schema.
    WrongType {
        /// serde's reason.
        reason: String,
    },
    /// A number outside its range.
    OutOfRange {
        /// The value given.
        got: String,
        /// The smallest allowed.
        min: String,
        /// The largest allowed.
        max: String,
    },
    /// A rule the type cannot say failed.
    Invalid {
        /// One English sentence.
        reason: String,
    },
    /// The key's `InProfile` is `Excluded` (or it is a `Machine` key), so a profile cannot hold it.
    NotInProfile,
    /// A secret reference was malformed.
    BadSecretRef {
        /// What is wrong with the label.
        reason: String,
    },
    /// An old stored value could not be migrated.
    Migration {
        /// The version it was stored at.
        from: u32,
        /// Why.
        reason: String,
    },
    /// No profile has this id.
    UnknownProfile {
        /// The id given.
        id: String,
    },
    /// A profile with this name exists and `overwrite` was not set.
    ProfileExists {
        /// The name.
        name: String,
    },
    /// A profile name is empty, too long or has control characters.
    BadProfileName {
        /// What is wrong.
        reason: String,
    },
    /// Storage failed (a store with files only; the mock never returns it).
    Storage {
        /// The reason, with no secret and no home folder path.
        reason: String,
    },
}

impl SettingErrorKind {
    /// The stable id the message id is derived from (`out_of_range` becomes `setting-error-out-of-range`).
    #[must_use]
    pub const fn id(&self) -> &'static str {
        match self {
            Self::UnknownKey => "unknown_key",
            Self::WrongType { .. } => "wrong_type",
            Self::OutOfRange { .. } => "out_of_range",
            Self::Invalid { .. } => "invalid",
            Self::NotInProfile => "not_in_profile",
            Self::BadSecretRef { .. } => "bad_secret_ref",
            Self::Migration { .. } => "migration",
            Self::UnknownProfile { .. } => "unknown_profile",
            Self::ProfileExists { .. } => "profile_exists",
            Self::BadProfileName { .. } => "bad_profile_name",
            Self::Storage { .. } => "storage",
        }
    }
}

impl SettingError {
    /// An error of `kind` for `key`.
    #[must_use]
    pub fn new(key: impl Into<String>, kind: SettingErrorKind) -> Self {
        Self { key: key.into(), kind }
    }

    /// No such key.
    #[must_use]
    pub fn unknown_key(key: impl Into<String>) -> Self {
        Self::new(key, SettingErrorKind::UnknownKey)
    }

    /// The value has the wrong type or shape.
    #[must_use]
    pub fn wrong_type(key: impl Into<String>, reason: impl fmt::Display) -> Self {
        Self::new(key, SettingErrorKind::WrongType { reason: reason.to_string() })
    }

    /// A rule failed, in one English sentence.
    #[must_use]
    pub fn invalid(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(key, SettingErrorKind::Invalid { reason: reason.into() })
    }

    /// `Ok` when `min <= value <= max`, else an [`OutOfRange`](SettingErrorKind::OutOfRange) naming `key`. Written for
    /// `Setting::check`: `SettingError::range("ui.scale", *v, 75, 200)`.
    ///
    /// # Errors
    /// The out-of-range error.
    pub fn range<T: Copy + PartialOrd + fmt::Display>(key: &str, value: T, min: T, max: T) -> Result<(), Self> {
        if value < min || value > max {
            let kind =
                SettingErrorKind::OutOfRange { got: value.to_string(), min: min.to_string(), max: max.to_string() };
            return Err(Self::new(key, kind));
        }
        Ok(())
    }

    fn english(&self) -> String {
        let key = &self.key;
        match &self.kind {
            SettingErrorKind::UnknownKey => format!("no setting named `{key}`"),
            SettingErrorKind::WrongType { reason } => format!("`{key}` has the wrong type: {reason}"),
            SettingErrorKind::OutOfRange { got, min, max } => {
                format!("`{key}` = {got} is out of range {min} to {max}")
            }
            SettingErrorKind::Invalid { reason } => format!("`{key}` is invalid: {reason}"),
            SettingErrorKind::NotInProfile => format!("`{key}` cannot be saved in a profile"),
            SettingErrorKind::BadSecretRef { reason } => format!("`{key}` holds a bad secret reference: {reason}"),
            SettingErrorKind::Migration { from, reason } => {
                format!("`{key}` stored at version {from} could not be migrated: {reason}")
            }
            SettingErrorKind::UnknownProfile { id } => format!("no profile `{id}`"),
            SettingErrorKind::ProfileExists { name } => format!("a profile named `{name}` already exists"),
            SettingErrorKind::BadProfileName { reason } => format!("bad profile name: {reason}"),
            SettingErrorKind::Storage { reason } => format!("settings storage failed: {reason}"),
        }
    }
}

/// A stored value that was not used, reported instead of deleted (D5). The file keeps the value; the key reads as
/// its default; the log and the Settings page show the finding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SettingFinding {
    /// Why the stored value was not used.
    pub error: SettingError,
    /// The stored value, so the page can show what was ignored. Never a secret: values cannot hold one.
    pub stored: Value,
    /// The file layer it came from, such as `settings.toml` or `profiles/stream-k3f9.toml`.
    pub from: String,
}

impl SettingFinding {
    /// A finding about `stored`, read from `from`.
    #[must_use]
    pub fn new(error: SettingError, stored: Value, from: impl Into<String>) -> Self {
        Self { error, stored, from: from.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_names_the_key_and_the_bounds() {
        assert_eq!(SettingError::range("ui.scale", 100, 75, 200), Ok(()));
        let e = SettingError::range("ui.scale", 900, 75, 200).unwrap_err();
        assert_eq!(e.to_string(), "`ui.scale` = 900 is out of range 75 to 200");
        assert_eq!(e.kind.id(), "out_of_range");
    }

    #[test]
    fn errors_round_trip_through_json() {
        let e = SettingError::wrong_type("ui.scale", "expected u16");
        let back: SettingError = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }
}
