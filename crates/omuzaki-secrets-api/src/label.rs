//! [`SecretLabel`]: the name of a secret.

use std::fmt;

use crate::error::SecretError;

/// The name of a secret, such as `automation/streamerbot` or `connector/openai`. It is the same label the settings
/// design's `SecretRef` carries; when both land, `SecretRef` wraps a `SecretLabel`.
///
/// A label is not secret: it can be shown, logged and written to a document. The shape is the one `SecretRef` checks:
/// 1 to [`MAX_LEN`](Self::MAX_LEN) characters of `a-z`, `0-9`, `-`, `_`, `.` and `/`, with no empty segment and no `.`
/// or `..` segment, so a label cannot be mistaken for a setting key or climb out of its folder.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretLabel(String);

impl SecretLabel {
    /// The longest label.
    pub const MAX_LEN: usize = 128;

    /// Validates `label`.
    ///
    /// # Errors
    /// [`SecretError::BadLabel`] says which rule the text breaks.
    pub fn new(label: impl Into<String>) -> Result<Self, SecretError> {
        let label = label.into();
        match Self::problem(&label) {
            None => Ok(Self(label)),
            Some(reason) => Err(SecretError::bad_label(label, reason)),
        }
    }

    fn problem(label: &str) -> Option<String> {
        if label.is_empty() || label.len() > Self::MAX_LEN {
            return Some(format!("a label is 1 to {} characters", Self::MAX_LEN));
        }
        if !label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_./".contains(c)) {
            return Some("use a-z, 0-9, '-', '_', '.' and '/'".to_owned());
        }
        if label.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
            return Some("no empty segment, '.' or '..'".to_owned());
        }
        None
    }

    /// The label text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for SecretLabel {
    type Error = SecretError;
    fn try_from(label: &str) -> Result<Self, Self::Error> {
        Self::new(label)
    }
}

impl TryFrom<String> for SecretLabel {
    type Error = SecretError;
    fn try_from(label: String) -> Result<Self, Self::Error> {
        Self::new(label)
    }
}
