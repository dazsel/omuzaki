//! `keyring::Error` to [`SecretError`].

use omuzaki_secrets_api::SecretError;

/// What to do when there is no keychain.
fn unavailable_hint() -> &'static str {
    if cfg!(target_os = "linux") {
        "install and start a Secret Service provider (GNOME Keyring or KeePassXC) and sign in to a desktop session"
    } else {
        "the system keychain could not be opened"
    }
}

/// Maps a keychain error for the entry `what` (a label, or a description of Omuzaki's own entry).
///
/// Every arm names what it keeps. `BadEncoding`, `BadDataFormat` and `Ambiguous` carry the stored bytes or
/// entries, which are secret, so none of them is formatted: the result is built from `what` and fixed text only.
/// `NoEntry` is not an error to the callers that read, so they handle it before calling this.
pub(crate) fn map_error(error: keyring::Error, what: &str) -> SecretError {
    use keyring::Error;
    match error {
        Error::NoDefaultStore | Error::NotSupportedByStore(_) => SecretError::unavailable(unavailable_hint()),
        Error::NoStorageAccess(cause) => SecretError::denied(cause.to_string()),
        Error::PlatformFailure(cause) => SecretError::backend(cause.to_string()),
        Error::Invalid(attribute, reason) => SecretError::rejected(format!("{attribute} {reason}")),
        Error::TooLong(attribute, limit) => {
            SecretError::rejected(format!("{attribute} is longer than the platform limit of {limit}"))
        }
        Error::BadEncoding(_) | Error::BadDataFormat(..) | Error::BadStoreFormat(_) => SecretError::corrupt(what),
        Error::Ambiguous(_) => SecretError::backend(format!("more than one keychain entry matches {what}")),
        Error::NoEntry => SecretError::backend(format!("the keychain has no entry for {what}")),
        // `keyring::Error` is non-exhaustive; an unknown variant could carry stored bytes, so it is not formatted.
        _ => SecretError::backend("the keychain reported an error this version does not know"),
    }
}

#[cfg(test)]
mod tests {
    use keyring::Error;

    use super::*;

    /// Stands for a stored secret that a keyring error carries.
    const CANARY: &str = "canary-secret-4471";

    fn canary_bytes() -> Vec<u8> {
        CANARY.as_bytes().to_vec()
    }

    fn platform(text: &str) -> Box<dyn std::error::Error + Send + Sync> {
        text.into()
    }

    /// Every `keyring::Error` that can hold stored bytes or entries, and every other variant.
    fn every_keyring_error() -> Vec<Error> {
        vec![
            Error::PlatformFailure(platform("dbus failed")),
            Error::NoStorageAccess(platform("locked")),
            Error::NoEntry,
            Error::BadEncoding(canary_bytes()),
            Error::BadDataFormat(canary_bytes(), platform("bad blob")),
            Error::BadStoreFormat("bad store".into()),
            Error::TooLong("user".into(), 255),
            Error::Invalid("user".into(), "is empty".into()),
            Error::Ambiguous(Vec::new()),
            Error::NoDefaultStore,
            Error::NotSupportedByStore("no".into()),
        ]
    }

    #[test]
    fn no_mapped_error_shows_the_stored_bytes() {
        for error in every_keyring_error() {
            let mapped = map_error(error, "automation/x");
            for text in [mapped.to_string(), format!("{mapped:?}")] {
                assert!(!text.contains(CANARY), "{text}");
            }
        }
    }

    #[test]
    fn unreadable_data_is_corrupt_and_names_what() {
        let mapped = map_error(Error::BadEncoding(canary_bytes()), "automation/x");
        assert_eq!(mapped, SecretError::corrupt("automation/x"));
    }

    #[test]
    fn a_missing_keychain_is_unavailable_with_a_hint() {
        let SecretError::Unavailable { hint, .. } = map_error(Error::NoDefaultStore, "a/b") else {
            panic!("NoDefaultStore must be Unavailable");
        };
        assert_ne!(hint, "");
    }

    #[test]
    fn a_locked_keychain_is_denied_and_a_platform_limit_is_rejected() {
        assert!(matches!(map_error(Error::NoStorageAccess(platform("locked")), "a/b"), SecretError::Denied { .. }));
        assert!(matches!(map_error(Error::TooLong("user".into(), 9), "a/b"), SecretError::Rejected { .. }));
        assert!(matches!(map_error(Error::Invalid("user".into(), "x".into()), "a/b"), SecretError::Rejected { .. }));
    }
}
