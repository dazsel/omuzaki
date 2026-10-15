//! `MemorySecrets` against the conformance suite, and its own failure switch.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use std::sync::Arc;

use omuzaki_secrets_api::{SecretError, SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::{MemorySecrets, conformance};

fn label(text: &str) -> SecretLabel {
    SecretLabel::new(text).unwrap()
}

#[test]
fn passes_the_conformance_suite() {
    conformance::run(&MemorySecrets::new(), "test/mock");
}

#[test]
fn passes_through_a_trait_object() {
    let store: Arc<dyn SecretStore> = Arc::new(MemorySecrets::new());
    conformance::run(&*store, "test/dyn");
}

#[test]
fn a_failing_store_fails_every_call_and_changes_nothing() {
    let store = MemorySecrets::new();
    let a = label("a/b");
    store.set(&a, SecretValue::new("v")).unwrap();

    let down = SecretError::unavailable("no daemon");
    store.fail_with(down.clone());
    assert_eq!(store.get(&a).err(), Some(down.clone()));
    assert_eq!(store.contains(&a).err(), Some(down.clone()));
    assert_eq!(store.labels().err(), Some(down.clone()));
    assert_eq!(store.set(&a, SecretValue::new("w")).err(), Some(down.clone()));
    assert_eq!(store.delete(&a).err(), Some(down));

    store.clear_failure();
    assert_eq!(store.get(&a).unwrap().unwrap().expose(), "v", "nothing changed while it failed");
}

#[test]
fn debug_lists_labels_and_never_values() {
    let store = MemorySecrets::new();
    store.set(&label("a/b"), SecretValue::new("canary-value-9317")).unwrap();
    let text = format!("{store:?}");
    assert!(text.contains("a/b"));
    assert!(!text.contains("canary-value-9317"));
}
