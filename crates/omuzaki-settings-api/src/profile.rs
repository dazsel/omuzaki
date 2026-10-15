//! Profiles (D7): named overlays of `User` keys. This module holds the identity and the naming rules; the overlay
//! itself is resolved by the service.

use omuzaki_core::slug_id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{SettingError, SettingErrorKind};

slug_id!(
    /// A profile's stable id and file name stem (`stream-k3f9`): an ASCII slug plus four characters, so renaming the
    /// profile never touches the file name and no name can be a path.
    ProfileId
);

impl ProfileId {
    /// The longest slug part kept from a name, leaving room for `-` and the suffix inside the slug limit.
    const STEM_MAX: usize = 32;

    /// The id for a new profile called `name`: its ASCII letters and digits as a slug, then `-suffix`. A name with no
    /// ASCII (a Japanese name) gets the stem `profile`. The caller supplies the four-character `suffix` (random in the
    /// real store, a counter in the mock), so this stays a pure function.
    ///
    /// # Errors
    /// [`SettingErrorKind::BadProfileName`] if `suffix` is not 4 lowercase letters or digits.
    pub fn from_name(name: &str, suffix: &str) -> Result<Self, SettingError> {
        let bad = |reason: &str| SettingError::new("", SettingErrorKind::BadProfileName { reason: reason.into() });
        if suffix.len() != 4 || !suffix.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return Err(bad("the id suffix is 4 lowercase letters or digits"));
        }
        let mut stem = String::new();
        for c in name.chars().map(|c| c.to_ascii_lowercase()) {
            if c.is_ascii_alphanumeric() {
                stem.push(c);
            } else if !stem.is_empty() && !stem.ends_with('-') {
                stem.push('-');
            }
        }
        let stem = stem.trim_end_matches('-');
        let stem = if stem.is_empty() { "profile" } else { &stem[..stem.len().min(Self::STEM_MAX)] };
        Self::new(format!("{}-{suffix}", stem.trim_end_matches('-'))).map_err(|e| bad(&e.to_string()))
    }
}

/// What the profile list shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ProfileInfo {
    /// The stable id.
    pub id: ProfileId,
    /// The display name (any text, 1 to 48 characters).
    pub name: String,
    /// How many keys the profile overrides.
    pub keys: u32,
    /// Whether it is the active profile.
    pub active: bool,
}

impl ProfileInfo {
    /// A profile entry.
    #[must_use]
    pub fn new(id: ProfileId, name: impl Into<String>, keys: u32, active: bool) -> Self {
        Self { id, name: name.into(), keys, active }
    }
}

/// Where a new profile's values come from (`settings.profile.save { from }`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProfileSource {
    /// The effective values now: every `Member` key whose effective value differs from base-or-default.
    Current,
    /// Duplicate this profile.
    Profile(ProfileId),
}

/// The longest display name, in characters.
pub const NAME_MAX: usize = 48;

/// Trims `name` and checks it: 1 to [`NAME_MAX`] characters, no control characters. Unicode normalisation (NFC) is the
/// store's job, because it needs a table this contract crate does not carry.
///
/// # Errors
/// [`SettingErrorKind::BadProfileName`].
pub fn validate_name(name: &str) -> Result<String, SettingError> {
    let bad = |reason: &str| SettingError::new("", SettingErrorKind::BadProfileName { reason: reason.into() });
    let name = name.trim();
    match name.chars().count() {
        0 => Err(bad("a name can't be empty")),
        n if n > NAME_MAX => Err(bad("a name is at most 48 characters")),
        _ if name.chars().any(char::is_control) => Err(bad("a name can't contain control characters")),
        _ => Ok(name.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_slugs_with_a_suffix() {
        assert_eq!(ProfileId::from_name("Stream  Setup!", "k3f9").unwrap().as_str(), "stream-setup-k3f9");
        assert_eq!(ProfileId::from_name("配信", "k3f9").unwrap().as_str(), "profile-k3f9");
        assert_eq!(ProfileId::from_name(&"a".repeat(60), "k3f9").unwrap().as_str().len(), 32 + 5);
        assert!(ProfileId::from_name("x", "K3F9").is_err());
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(validate_name("  配信 用 ").unwrap(), "配信 用");
        assert!(validate_name("  ").is_err());
        assert!(validate_name(&"あ".repeat(49)).is_err());
        assert!(validate_name(&"あ".repeat(48)).is_ok());
        assert!(validate_name("a\nb").is_err());
    }
}
