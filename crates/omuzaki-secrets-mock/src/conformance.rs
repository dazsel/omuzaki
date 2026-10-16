//! The conformance suite: the behaviour every [`SecretStore`] has, run against the mock here and against the real
//! keychain in `omuzaki-secrets`.

// A conformance suite reports a broken store by panicking with the failed check, like an `assert!`, so it may unwrap.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_secrets_api::error::SecretError;
use omuzaki_secrets_api::label::SecretLabel;
use omuzaki_secrets_api::store::SecretStore;
use omuzaki_secrets_api::value::SecretValue;

/// Removes the suite's labels when it ends, also when an assertion fails, so a real keychain is left as it was.
struct Cleanup<'a, S: SecretStore + ?Sized> {
    store: &'a S,
    labels: Vec<SecretLabel>,
}

impl<S: SecretStore + ?Sized> Drop for Cleanup<'_, S> {
    fn drop(&mut self) {
        for label in &self.labels {
            // Best effort: the run may already be failing, and a second panic in `drop` would hide the first.
            let _ = self.store.delete(label);
        }
    }
}

fn label(prefix: &str, name: &str) -> SecretLabel {
    SecretLabel::new(format!("{prefix}/{name}")).unwrap()
}

fn read(store: &(impl SecretStore + ?Sized), label: &SecretLabel) -> Option<String> {
    store.get(label).unwrap().map(|v| v.expose().to_owned())
}

/// Runs the suite against `store`. Every label it writes starts with `prefix` (a valid label such as
/// `test/conformance-1234`, unique per run) and is deleted afterwards, so a real keychain keeps no residue and the
/// run does not disturb other entries.
///
/// It checks: a missing label (`get` is `None`, `contains` is false, `delete` is not an error); `set` then `get`,
/// including a value with spaces and non-ASCII text; replacing a value; `labels` listing exactly the labels set, in
/// order, and nothing after a delete; labels not disturbing each other; and an empty value being
/// [`SecretError::Rejected`] without damaging the old value.
///
/// # Panics
/// With a message naming the check that failed.
pub fn run(store: &(impl SecretStore + ?Sized), prefix: &str) {
    let first = label(prefix, "first");
    let second = label(prefix, "second");
    let _cleanup = Cleanup { store, labels: vec![first.clone(), second.clone()] };
    let mine = || -> Vec<SecretLabel> {
        store.labels().unwrap().into_iter().filter(|l| l.as_str().starts_with(&format!("{prefix}/"))).collect()
    };

    // A missing label.
    assert_eq!(read(store, &first), None, "get of a missing label is None");
    assert!(!store.contains(&first).unwrap(), "contains of a missing label is false");
    store.delete(&first).expect("delete of a missing label is not an error");
    assert!(mine().is_empty(), "no label before any set");

    // Set and get.
    store.set(&first, SecretValue::new("token one")).unwrap();
    assert_eq!(read(store, &first).as_deref(), Some("token one"), "get returns what set stored");
    assert!(store.contains(&first).unwrap(), "contains after set");
    assert_eq!(mine(), std::slice::from_ref(&first), "labels lists the label set");

    // Replace.
    store.set(&first, SecretValue::new("tökën \u{2603} two")).unwrap();
    assert_eq!(read(store, &first).as_deref(), Some("tökën \u{2603} two"), "set replaces, with any text");
    assert_eq!(mine(), std::slice::from_ref(&first), "replacing adds no second label");

    // Two labels are independent, and listed in order.
    store.set(&second, SecretValue::new("other")).unwrap();
    assert_eq!(mine(), [first.clone(), second.clone()], "labels are in order");
    assert_eq!(read(store, &first).as_deref(), Some("tökën \u{2603} two"), "a second label leaves the first alone");

    // An empty value is refused and changes nothing.
    let refused = store.set(&first, SecretValue::new("")).unwrap_err();
    assert!(matches!(refused, SecretError::Rejected { .. }), "an empty value is Rejected, got {refused:?}");
    assert_eq!(read(store, &first).as_deref(), Some("tökën \u{2603} two"), "a refused set keeps the old value");

    // Delete.
    store.delete(&first).unwrap();
    assert_eq!(read(store, &first), None, "get after delete is None");
    assert!(!store.contains(&first).unwrap(), "contains after delete is false");
    assert_eq!(mine(), std::slice::from_ref(&second), "labels after delete");
    assert_eq!(read(store, &second).as_deref(), Some("other"), "deleting one leaves the other");
    store.delete(&first).expect("deleting twice is not an error");
    store.delete(&second).unwrap();
    assert!(mine().is_empty(), "nothing left");
}
