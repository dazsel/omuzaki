//! The index logic over an in-memory [`Backend`], through the shared conformance suite.

#![allow(clippy::assert_is_empty)] // `labels()` and `users()` are Vecs; `assert_eq!` against `[]` needs a type annotation

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use omuzaki_secrets_mock::conformance;

use super::*;

/// A keychain in a map that can be told to fail a call.
#[derive(Default)]
struct MapBackend {
    entries: Mutex<BTreeMap<String, String>>,
    /// Fail the call with this number (counting every call from 0), if set.
    fail_call: Mutex<Option<usize>>,
    calls: Mutex<usize>,
    /// The longest text an entry may hold (Windows Credential Manager: 2,560 bytes); longer is `TooLong`.
    max_len: Option<usize>,
    /// Drop the next write to this entry, as another program's write landing between our read and write would.
    lose_next_write_to: Mutex<Option<String>>,
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

/// Several stores over one keychain.
impl Backend for Arc<MapBackend> {
    fn get(&self, user: &str) -> Result<Option<SecretValue>, SecretError> {
        (**self).get(user)
    }

    fn set(&self, user: &str, text: &str) -> Result<(), SecretError> {
        (**self).set(user, text)
    }

    fn remove(&self, user: &str) -> Result<(), SecretError> {
        (**self).remove(user)
    }
}

impl Backend for MapBackend {
    fn get(&self, user: &str) -> Result<Option<SecretValue>, SecretError> {
        self.tick()?;
        Ok(self.entries.lock().unwrap().get(user).map(SecretValue::new))
    }

    fn set(&self, user: &str, text: &str) -> Result<(), SecretError> {
        self.tick()?;
        if self.max_len.is_some_and(|max| text.len() > max) {
            return Err(SecretError::rejected("too long"));
        }
        let mut lost = self.lose_next_write_to.lock().unwrap();
        if lost.as_deref() == Some(user) {
            *lost = None;
            return Ok(());
        }
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
    assert_eq!(store.backend.users(), ["@labels.0", "a/b"]);
    store.delete(&label("a/b")).unwrap();
    assert!(store.backend.users().is_empty());
}

#[test]
fn an_unreadable_index_is_corrupt_not_skipped() {
    let store = Indexed::new(MapBackend::default());
    store.backend.set("@labels.0", "a/b\nNOT A LABEL").unwrap();
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

#[test]
fn a_blank_record_or_an_empty_shard_in_the_index_is_corrupt() {
    for text in ["", "a/b\n\nc/d", "a/b\n"] {
        let store = Indexed::new(MapBackend::default());
        store.backend.set("@labels.0", text).unwrap();
        assert_eq!(store.labels().unwrap_err(), SecretError::corrupt(INDEX_WHAT), "{text:?}");
    }
}

fn long_label(i: usize) -> SecretLabel {
    let head = format!("a/{i:04}-");
    label(&format!("{head}{}", "x".repeat(SecretLabel::MAX_LEN - head.len())))
}

#[test]
fn many_labels_are_listed_on_a_keychain_that_caps_an_entry_at_2560_bytes() {
    // Twenty maximum-length labels were one 2,579-byte entry; the twentieth set failed and was never listed.
    let store = Indexed::new(MapBackend { max_len: Some(2_560), ..MapBackend::default() });
    let all: Vec<_> = (0..40).map(long_label).collect();
    for l in &all {
        store.set(l, SecretValue::new("v")).unwrap();
    }
    assert_eq!(store.labels().unwrap(), all);
    for l in &all {
        assert!(store.contains(l).unwrap());
    }
}

#[test]
fn a_shrinking_list_drops_the_shards_it_no_longer_needs() {
    let store = Indexed::new(MapBackend::default());
    let all: Vec<_> = (0..40).map(long_label).collect();
    for l in &all {
        store.set(l, SecretValue::new("v")).unwrap();
    }
    let shards = |s: &Indexed<MapBackend>| s.backend.users().iter().filter(|u| is_shard_user(u)).count();
    assert!(shards(&store) > 2);
    for l in &all[1..] {
        store.delete(l).unwrap();
    }
    assert_eq!(shards(&store), 1);
    assert_eq!(store.labels().unwrap(), [all[0].clone()]);
    store.delete(&all[0]).unwrap();
    assert_eq!(shards(&store), 0);
    assert!(store.backend.users().is_empty());
}

#[test]
fn a_write_another_program_overwrote_is_made_again() {
    let store = Indexed::new(MapBackend::default());
    store.set(&label("a/one"), SecretValue::new("v")).unwrap();
    *store.backend.lose_next_write_to.lock().unwrap() = Some("@labels.0".to_owned());
    store.set(&label("a/two"), SecretValue::new("v")).unwrap();
    assert_eq!(store.labels().unwrap(), [label("a/one"), label("a/two")]);
}

#[test]
fn separate_handles_over_one_keychain_do_not_lose_each_others_labels() {
    let keychain = Arc::new(MapBackend::default());
    let lock = Arc::new(Mutex::new(()));
    let handles: Vec<_> = (0..8).map(|_| Indexed::sharing(Arc::clone(&keychain), Arc::clone(&lock))).collect();
    std::thread::scope(|scope| {
        for (n, handle) in handles.iter().enumerate() {
            scope.spawn(move || {
                for i in 0..25 {
                    handle.set(&label(&format!("t{n}/l{i}")), SecretValue::new("v")).unwrap();
                }
            });
        }
    });
    assert_eq!(handles[0].labels().unwrap().len(), 8 * 25);
}

#[test]
fn every_keychain_store_in_the_process_shares_one_index_lock() {
    let (a, b) = (KeychainSecrets::new(), KeychainSecrets::default());
    assert!(Arc::ptr_eq(&a.0.index_lock, &b.0.index_lock));
}
