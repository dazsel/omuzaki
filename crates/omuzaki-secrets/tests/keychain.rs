//! The real OS keychain. CI on Linux has no Secret Service daemon, so the tests that need one are `#[ignore]`d and
//! the owner runs them on a desktop machine:
//!
//! ```text
//! cargo test -p omuzaki-secrets --test keychain -- --ignored
//! ```
//!
//! They use a label prefix unique to the run and delete what they write.

use omuzaki_secrets::KeychainSecrets;
use omuzaki_secrets_api::{SecretError, SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::conformance;

fn unique_prefix() -> String {
    format!("test/keychain-{}", std::process::id())
}

#[test]
#[ignore = "needs an OS keychain (Credential Manager, Keychain, or a Secret Service daemon): owner-run"]
fn the_real_keychain_passes_the_conformance_suite() {
    conformance::run(&KeychainSecrets::new(), &unique_prefix());
}

#[test]
#[ignore = "needs an OS keychain (Credential Manager, Keychain, or a Secret Service daemon): owner-run"]
fn a_value_survives_a_new_store_handle() {
    let label = SecretLabel::new(format!("{}/persist", unique_prefix())).unwrap();
    KeychainSecrets::new().set(&label, SecretValue::new("kept")).unwrap();
    let again = KeychainSecrets::new();
    let read = again.get(&label).unwrap().map(|v| v.expose().to_owned());
    again.delete(&label).unwrap();
    assert_eq!(read.as_deref(), Some("kept"));
}

/// On a machine without a Secret Service daemon (CI) every call is `Unavailable` with a hint; with one, the call
/// works. Either way it never panics and never falls back to a file.
#[test]
#[cfg(target_os = "linux")]
fn without_a_keychain_the_store_is_unavailable_not_a_file() {
    let store = KeychainSecrets::new();
    let label = SecretLabel::new(format!("{}/probe", unique_prefix())).unwrap();
    match store.contains(&label) {
        Ok(false) => {}
        Err(SecretError::Unavailable { hint, .. }) => assert_ne!(hint, ""),
        other => panic!("expected a missing label or Unavailable, got {other:?}"),
    }
}
