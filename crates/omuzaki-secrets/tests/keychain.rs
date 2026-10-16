//! The real OS keychain. CI on Linux has no Secret Service daemon, so the tests that need one are `#[ignore]`d and
//! the owner runs them on a desktop machine (Windows, macOS, or Linux with a Secret Service provider):
//!
//! ```text
//! cargo test -p omuzaki-secrets --test keychain -- --ignored
//! ```
//!
//! Each test uses a namespace of its own, unique to the run (its own keychain service and lock file), so it shares
//! no label list with the other tests or with the app's secrets, and deletes what it writes. They can run at the
//! same time.

use omuzaki_secrets::KeychainSecrets;
use omuzaki_secrets_api::error::SecretError;
use omuzaki_secrets_api::label::SecretLabel;
use omuzaki_secrets_api::store::SecretStore;
use omuzaki_secrets_api::value::SecretValue;
use omuzaki_secrets_mock::conformance;

/// A store in a namespace for the test called `test`, unique to this run, with its lock file in `data`.
fn store(test: &str, data: &tempfile::TempDir) -> Result<KeychainSecrets, SecretError> {
    KeychainSecrets::in_namespace(&format!("test-{test}-{}", std::process::id()), data.path())
}

#[test]
#[ignore = "needs an OS keychain (Credential Manager, Keychain, or a Secret Service daemon): owner-run"]
fn the_real_keychain_passes_the_conformance_suite() {
    let data = tempfile::tempdir().unwrap();
    conformance::run(&store("conformance", &data).unwrap(), "test/conformance");
}

#[test]
#[ignore = "needs an OS keychain (Credential Manager, Keychain, or a Secret Service daemon): owner-run"]
fn a_value_survives_a_new_store_handle() {
    let data = tempfile::tempdir().unwrap();
    let label = SecretLabel::new("test/persist").unwrap();
    store("persist", &data).unwrap().set(&label, SecretValue::new("kept")).unwrap();
    let again = store("persist", &data).unwrap();
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
    let store = store("long", &data).unwrap();
    let labels: Vec<_> = (0..12)
        .map(|i| {
            let head = format!("test/{i:02}-");
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
    assert_eq!(store.labels().unwrap().len(), 0);
}

/// On a machine without a Secret Service daemon (CI) every call is `Unavailable` with a hint; with one, the call
/// works. Either way it never panics and never falls back to a file.
#[test]
#[cfg(target_os = "linux")]
fn without_a_keychain_the_store_is_unavailable_not_a_file() {
    let data = tempfile::tempdir().unwrap();
    let store = store("probe", &data).unwrap();
    let label = SecretLabel::new("test/probe").unwrap();
    match store.contains(&label) {
        Ok(false) => {}
        Err(SecretError::Unavailable { hint, .. }) => assert_ne!(hint, ""),
        other => panic!("expected a missing label or Unavailable, got {other:?}"),
    }
}
