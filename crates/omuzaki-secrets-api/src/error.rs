//! [`SecretError`]: why a secret operation failed.

/// Why a [`SecretStore`](crate::store::SecretStore) call failed. No variant holds a secret value, and none of the
/// `Display` texts can print one: stores build these from the label and the platform's own message only.
///
/// The `Display` text is written for the person who sees it, saying what to do next. Every variant is
/// `#[non_exhaustive]` and has a constructor, so a field can be added without breaking implementers.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// There is no keychain to talk to (for example Linux without a Secret Service daemon). Nothing was read or
    /// written, and the store never falls back to a file.
    #[error("no secret store is available: {hint}")]
    #[non_exhaustive]
    Unavailable {
        /// What the person can do about it.
        hint: String,
    },
    /// The keychain refused access (locked, or the person said no).
    #[error("the keychain refused access ({reason}); unlock it and try again")]
    #[non_exhaustive]
    Denied {
        /// The platform's reason.
        reason: String,
    },
    /// The store cannot take this request (a label or value the platform rejects, or an empty value).
    #[error("the secret was not stored: {reason}")]
    #[non_exhaustive]
    Rejected {
        /// Why.
        reason: String,
    },
    /// A label is not a valid [`SecretLabel`](crate::label::SecretLabel).
    #[error("`{label}` is not a valid secret label: {reason}")]
    #[non_exhaustive]
    BadLabel {
        /// The text that was refused. A label is not secret.
        label: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The keychain holds data for `what` that Omuzaki cannot read (not text, or not in the expected shape).
    #[error("the keychain data for {what} cannot be read; delete it and set it again")]
    #[non_exhaustive]
    Corrupt {
        /// The label, or a description of Omuzaki's own bookkeeping entry.
        what: String,
    },
    /// The platform failed in some other way.
    #[error("the keychain failed: {message}")]
    #[non_exhaustive]
    Backend {
        /// The platform's message.
        message: String,
    },
}

impl SecretError {
    /// No keychain is available; `hint` says what to do.
    pub fn unavailable(hint: impl Into<String>) -> Self {
        Self::Unavailable { hint: hint.into() }
    }

    /// The keychain refused access.
    pub fn denied(reason: impl Into<String>) -> Self {
        Self::Denied { reason: reason.into() }
    }

    /// The store cannot take the request.
    pub fn rejected(reason: impl Into<String>) -> Self {
        Self::Rejected { reason: reason.into() }
    }

    /// `label` is not a valid label.
    pub fn bad_label(label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::BadLabel { label: label.into(), reason: reason.into() }
    }

    /// The stored data for `what` is unreadable.
    pub fn corrupt(what: impl Into<String>) -> Self {
        Self::Corrupt { what: what.into() }
    }

    /// Some other platform failure.
    pub fn backend(message: impl Into<String>) -> Self {
        Self::Backend { message: message.into() }
    }
}
