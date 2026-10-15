//! The index logic over an in-memory [`Backend`], through the shared conformance suite.

#![allow(clippy::assert_is_empty)] // `labels()` and `users()` are Vecs; `assert_eq!` against `[]` needs a type annotation

use std::collections::BTreeMap;
use std::sync::Mutex;

use omuzaki_secrets_mock::conformance;

use super::*;

/// A keychain in a map that can be told to fail a call.
#[derive(Default)]
struct MapBackend {
    entries: Mutex<BTreeMap<String, String>>,
    /// Fail the call with this number (counting every call from 0), if set.
    fail_call: Mutex<Option<usize>>,
    calls: Mutex<usize>,
}

impl MapBackend {
    fn tick(&self) -> Result<(), SecretError> {
        let mut calls = self.calls.lock().unwrap();
        let n = *calls;
        *calls += 1;
        if *self.fail_call.lock().unwrap() == Some(n) {
            return Err(SecretError::backend("injected"));
        }
        Ok(())
    }

    fn fail_at(&self, n: usize) {
        *self.calls.lock().unwrap() = 0;
        *self.fail_call.lock().unwrap() = Some(n);
    }

    fn stop_failing(&self) {
        *self.fail_call.lock().unwrap() = None;
    }

    fn users(&self) -> Vec<String> {
        self.entries.lock().unwrap().keys().cloned().collect()
    }
}

impl Backend for MapBackend {
    fn get(&self, user: &str) -> Result<Option<SecretValue>, SecretError> {
        self.tick()?;
        Ok(self.entries.lock().unwrap().get(user).map(SecretValue::new))
    }

    fn set(&self, user: &str, text: &str) -> Result<(), SecretError> {
        self.tick()?;
        self.entries.lock().unwrap().insert(user.to_owned(), text.to_owned());
        Ok(())
    }

    fn remove(&self, user: &str) -> Result<(), SecretError> {
        self.tick()?;
        self.entries.lock().unwrap().remove(user);
        Ok(())
    }
}

fn label(text: &str) -> SecretLabel {
    SecretLabel::new(text).unwrap()
}

#[test]
fn passes_the_conformance_suite() {
    conformance::run(&Indexed::new(MapBackend::default()), "test/indexed");
}

#[test]
fn other_programs_entries_are_not_listed() {
    let store = Indexed::new(MapBackend::default());
    store.backend.set("someone-elses-entry", "x").unwrap();
    store.set(&label("a/b"), SecretValue::new("v")).unwrap();
    assert_eq!(store.labels().unwrap(), [label("a/b")]);
}

#[test]
fn deleting_the_last_label_removes_the_index_entry() {
    let store = Indexed::new(MapBackend::default());
    store.set(&label("a/b"), SecretValue::new("v")).unwrap();
    assert_eq!(store.backend.users(), [INDEX_USER, "a/b"]);
    store.delete(&label("a/b")).unwrap();
    assert!(store.backend.users().is_empty());
}

#[test]
fn an_unreadable_index_is_corrupt_not_skipped() {
    let store = Indexed::new(MapBackend::default());
    store.backend.set(INDEX_USER, "a/b\nNOT A LABEL").unwrap();
    assert_eq!(store.labels().unwrap_err(), SecretError::corrupt(INDEX_WHAT));
}

#[test]
fn a_set_that_fails_after_the_entry_leaves_it_readable_and_a_retry_lists_it() {
    let store = Indexed::new(MapBackend::default());
    // Calls: 0 set entry, 1 read index, 2 write index.
    store.backend.fail_at(2);
    assert!(store.set(&label("a/b"), SecretValue::new("v")).is_err());
    store.backend.stop_failing();
    assert_eq!(store.get(&label("a/b")).unwrap().unwrap().expose(), "v");
    assert!(store.labels().unwrap().is_empty());
    store.set(&label("a/b"), SecretValue::new("v")).unwrap();
    assert_eq!(store.labels().unwrap(), [label("a/b")]);
}

#[test]
fn a_delete_that_fails_after_the_entry_leaves_a_label_and_a_retry_clears_it() {
    let store = Indexed::new(MapBackend::default());
    store.set(&label("a/b"), SecretValue::new("v")).unwrap();
    // Calls: 0 remove entry, 1 read index, 2 write index.
    store.backend.fail_at(2);
    assert!(store.delete(&label("a/b")).is_err());
    store.backend.stop_failing();
    assert_eq!(store.labels().unwrap(), [label("a/b")]);
    assert!(!store.contains(&label("a/b")).unwrap());
    store.delete(&label("a/b")).unwrap();
    assert!(store.labels().unwrap().is_empty());
}

#[test]
fn an_empty_value_never_reaches_the_keychain() {
    let store = Indexed::new(MapBackend::default());
    assert!(matches!(store.set(&label("a/b"), SecretValue::new("")), Err(SecretError::Rejected { .. })));
    assert!(store.backend.users().is_empty());
}
