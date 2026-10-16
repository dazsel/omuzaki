//! The real OS keychain. CI on Linux has no Secret Service daemon, so the tests that need one are `#[ignore]`d and
//! the owner runs them on a desktop machine (Windows, macOS, or Linux with a Secret Service provider):
//!
//! ```text
//! cargo test -p omuzaki-secrets --test keychain -- --ignored
//! ```
//!
//! Each test uses a label prefix of its own, unique to the run, and deletes what it writes, so the tests can run at
//! the same time without seeing each other's labels.

use omuzaki_secrets::KeychainSecrets;
use omuzaki_secrets_api::error::SecretError;
use omuzaki_secrets_api::label::SecretLabel;
use omuzaki_secrets_api::store::SecretStore;
use omuzaki_secrets_api::value::SecretValue;
use omuzaki_secrets_mock::conformance;

/// A label prefix for the test called `test`, unique to this run.
fn prefix(test: &str) -> String {
    format!("test/{test}-{}", std::process::id())
}

#[test]
#[ignore = "needs an OS keychain (Credential Manager, Keychain, or a Secret Service daemon): owner-run"]
fn the_real_keychain_passes_the_conformance_suite() {
    let data = tempfile::tempdir().unwrap();
    conformance::run(&KeychainSecrets::new(data.path()), &prefix("conformance"));
}

#[test]
#[ignore = "needs an OS keychain (Credential Manager, Keychain, or a Secret Service daemon): owner-run"]
fn a_value_survives_a_new_store_handle() {
    let data = tempfile::tempdir().unwrap();
    let label = SecretLabel::new(format!("{}/persist", prefix("persist"))).unwrap();
    KeychainSecrets::new(data.path()).set(&label, SecretValue::new("kept")).unwrap();
    let again = KeychainSecrets::new(data.path());
    let read = again.get(&label).unwrap().map(|v| v.expose().to_owned());
    let listed = again.labels().unwrap().contains(&label);
    again.delete(&label).unwrap();
    assert_eq!(read.as_deref(), Some("kept"));
    assert!(listed);
}

#[test]
#[ignore = "needs an OS keychain (Credential Manager, Keychain, or a Secret Service daemon): owner-run"]
fn many_maximum_length_labels_are_all_listed() {
    // Ten labels of 128 characters were the first to overflow Windows' 2,560-byte credential limit.
    let data = tempfile::tempdir().unwrap();
    let store = KeychainSecrets::new(data.path());
    let base = prefix("long");
    let labels: Vec<_> = (0..12)
        .map(|i| {
            let head = format!("{base}/{i:02}-");
            SecretLabel::new(format!("{head}{}", "x".repeat(SecretLabel::MAX_LEN - head.len()))).unwrap()
        })
        .collect();
    for label in &labels {
        store.set(label, SecretValue::new("v")).unwrap();
    }
    let listed = store.labels().unwrap();
    for label in &labels {
        store.delete(label).unwrap();
    }
    assert!(labels.iter().all(|l| listed.contains(l)));
}

/// On a machine without a Secret Service daemon (CI) every call is `Unavailable` with a hint; with one, the call
/// works. Either way it never panics and never falls back to a file.
#[test]
#[cfg(target_os = "linux")]
fn without_a_keychain_the_store_is_unavailable_not_a_file() {
    let data = tempfile::tempdir().unwrap();
    let store = KeychainSecrets::new(data.path());
    let label = SecretLabel::new(format!("{}/probe", prefix("probe"))).unwrap();
    match store.contains(&label) {
        Ok(false) => {}
        Err(SecretError::Unavailable { hint, .. }) => assert_ne!(hint, ""),
        other => panic!("expected a missing label or Unavailable, got {other:?}"),
    }
}
