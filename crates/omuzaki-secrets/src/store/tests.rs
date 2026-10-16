//! The index logic over in-memory and on-disk [`Backend`]s, through the shared conformance suite.

#![allow(clippy::assert_is_empty)] // `labels()` and `users()` are Vecs; `assert_eq!` against `[]` needs a type annotation

use std::collections::BTreeMap;
use std::ops::Deref;
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use omuzaki_secrets_mock::conformance;
use tempfile::TempDir;

use super::*;
use crate::index::SHARD_BUDGET;

/// A keychain in a map that can be told to fail a call, and that refuses what Windows would.
#[derive(Default)]
struct MapBackend {
    entries: Mutex<BTreeMap<String, String>>,
    /// Fail the call with this number (counting every call from 0), if set.
    fail_call: Mutex<Option<usize>>,
    calls: Mutex<usize>,
    /// The longest entry in UTF-16 bytes (Windows Credential Manager: 2,560); a longer one is refused.
    max_utf16_bytes: Option<usize>,
    /// Sleep this long in every call, to give other threads their turn between a read and a write.
    pause: Option<Duration>,
}

impl MapBackend {
    fn windows_like() -> Self {
        Self { max_utf16_bytes: Some(2_560), ..Self::default() }
    }

    fn tick(&self) -> Result<(), SecretError> {
        if let Some(pause) = self.pause {
            thread::sleep(pause);
        }
        let mut calls = self.calls.lock().unwrap();
        let n = *calls;
        *calls += 1;
        if *self.fail_call.lock().unwrap() == Some(n) {
            return Err(SecretError::backend("injected"));
        }
        Ok(())
    }

    /// Counts calls from now and fails the one numbered `n`.
    fn fail_at(&self, n: usize) {
        *self.calls.lock().unwrap() = 0;
        *self.fail_call.lock().unwrap() = Some(n);
    }

    fn stop_failing(&self) {
        *self.fail_call.lock().unwrap() = None;
    }

    fn start_counting(&self) {
        *self.calls.lock().unwrap() = 0;
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }

    fn users(&self) -> Vec<String> {
        self.entries.lock().unwrap().keys().cloned().collect()
    }

    fn index_users(&self) -> Vec<String> {
        self.users().into_iter().filter(|u| is_index_user(u)).collect()
    }
}

impl Backend for MapBackend {
    fn get(&self, user: &str) -> Result<Option<SecretValue>, SecretError> {
        self.tick()?;
        Ok(self.entries.lock().unwrap().get(user).map(SecretValue::new))
    }

    fn set(&self, user: &str, text: &str) -> Result<(), SecretError> {
        self.tick()?;
        if self.max_utf16_bytes.is_some_and(|max| text.encode_utf16().count() * 2 > max) {
            return Err(SecretError::rejected("too long for this keychain"));
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

/// A keychain in a folder, one file per entry, that other processes can open too.
struct DirBackend {
    dir: PathBuf,
}

impl DirBackend {
    fn file(&self, user: &str) -> PathBuf {
        self.dir.join(user.bytes().fold(String::new(), |name, b| name + &format!("{b:02x}")))
    }
}

impl Backend for DirBackend {
    fn get(&self, user: &str) -> Result<Option<SecretValue>, SecretError> {
        // Slow, so another process gets between a read and the write that follows it.
        thread::sleep(Duration::from_micros(300));
        match fs::read_to_string(self.file(user)) {
            Ok(text) => Ok(Some(SecretValue::new(text))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SecretError::backend(e.to_string())),
        }
    }

    fn set(&self, user: &str, text: &str) -> Result<(), SecretError> {
        let temp = self.dir.join(format!("tmp-{}", std::process::id()));
        fs::write(&temp, text)
            .and_then(|()| fs::rename(&temp, self.file(user)))
            .map_err(|e| SecretError::backend(e.to_string()))
    }

    fn remove(&self, user: &str) -> Result<(), SecretError> {
        match fs::remove_file(self.file(user)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SecretError::backend(e.to_string())),
        }
    }
}

/// A store with a lock directory of its own, removed when the fixture is dropped.
struct Fixture<B: Backend> {
    store: Indexed<B>,
    dir: TempDir,
}

impl<B: Backend> Deref for Fixture<B> {
    type Target = Indexed<B>;

    fn deref(&self) -> &Indexed<B> {
        &self.store
    }
}

fn fixture<B: Backend>(backend: B) -> Fixture<B> {
    let dir = tempfile::tempdir().unwrap();
    Fixture { store: Indexed::new(backend, DirLock::new(dir.path())), dir }
}

fn label(text: &str) -> SecretLabel {
    SecretLabel::new(text).unwrap()
}

/// A label of the longest length there is; they sort by `i`.
fn long_label(i: usize) -> SecretLabel {
    let head = format!("a/{i:04}-");
    label(&format!("{head}{}", "x".repeat(SecretLabel::MAX_LEN - head.len())))
}

fn set_ok(store: &impl SecretStore, label: &SecretLabel) {
    store.set(label, SecretValue::new("v")).unwrap();
}

#[test]
fn passes_the_conformance_suite() {
    conformance::run(&*fixture(MapBackend::default()), "test/indexed");
}

#[test]
fn other_programs_entries_are_not_listed() {
    let store = fixture(MapBackend::default());
    store.backend.set("someone-elses-entry", "x").unwrap();
    set_ok(&*store, &label("a/b"));
    assert_eq!(store.labels().unwrap(), [label("a/b")]);
}

#[test]
fn deleting_the_last_label_removes_every_index_entry() {
    let store = fixture(MapBackend::default());
    set_ok(&*store, &label("a/b"));
    assert_eq!(store.backend.users(), ["@labels", "@labels.0.0", "a/b"]);
    store.delete(&label("a/b")).unwrap();
    assert!(store.backend.users().is_empty());
}

#[test]
fn an_unreadable_index_is_corrupt_not_skipped() {
    let corrupt = SecretError::corrupt(INDEX_WHAT);
    // (head, shard 0 of generation 0): a bad label, a blank record, a trailing newline, an empty shard, a bad head,
    // and a head with nothing behind it.
    let cases = [
        (Some("0"), Some("a/b\nNOT A LABEL")),
        (Some("0"), Some("a/b\n\nc/d")),
        (Some("0"), Some("a/b\n")),
        (Some("0"), Some("")),
        (Some("zero"), Some("a/b")),
        (Some("0"), None),
    ];
    for (head, shard) in cases {
        let store = fixture(MapBackend::default());
        store.backend.set(HEAD_USER, head.unwrap()).unwrap();
        if let Some(shard) = shard {
            store.backend.set(&shard_user(0, 0), shard).unwrap();
        }
        assert_eq!(store.labels().unwrap_err(), corrupt, "{head:?} {shard:?}");
    }
}

#[test]
fn an_empty_value_never_reaches_the_keychain() {
    let store = fixture(MapBackend::default());
    assert!(matches!(store.set(&label("a/b"), SecretValue::new("")), Err(SecretError::Rejected { .. })));
    assert!(store.backend.users().is_empty());
}

#[test]
fn ten_maximum_length_labels_and_more_are_listed_on_a_keychain_that_caps_an_entry_at_2560_utf16_bytes() {
    // Ten maximum-length labels are 2,578 bytes in UTF-16: the tenth set used to fail after storing its secret.
    for count in [9, 10, 11, 40] {
        let store = fixture(MapBackend::windows_like());
        let all: Vec<_> = (0..count).map(long_label).collect();
        for l in &all {
            set_ok(&*store, l);
        }
        assert_eq!(store.labels().unwrap(), all, "{count} labels");
        assert!(all.iter().all(|l| store.contains(l).unwrap()));
    }
}

#[test]
fn a_shrinking_list_drops_the_shards_it_no_longer_needs() {
    let store = fixture(MapBackend::default());
    let all: Vec<_> = (0..40).map(long_label).collect();
    for l in &all {
        set_ok(&*store, l);
    }
    assert!(store.backend.index_users().len() > 3, "a head and several shards");
    for l in &all[1..] {
        store.delete(l).unwrap();
    }
    assert_eq!(store.backend.index_users().len(), 2, "a head and one shard");
    assert_eq!(store.labels().unwrap(), [all[0].clone()]);
    store.delete(&all[0]).unwrap();
    assert!(store.backend.index_users().is_empty());
}

/// The labels, in a store of `base` labels, and `with` added or `without` taken away.
fn check_every_failure_step(
    change: impl Fn(&Fixture<MapBackend>) -> Result<(), SecretError>,
    before: &[SecretLabel],
    after: &[SecretLabel],
) {
    let build = || {
        let store = fixture(MapBackend::default());
        for l in before {
            set_ok(&*store, l);
        }
        store
    };
    // Count the calls of an unfailing run.
    let probe = build();
    probe.backend.start_counting();
    change(&probe).unwrap();
    let total = probe.backend.calls();
    assert_eq!(probe.labels().unwrap(), after);
    assert!(total > 3, "the change makes several keychain calls ({total})");

    for n in 0..total {
        let store = build();
        store.backend.fail_at(n);
        let result = change(&store);
        store.backend.stop_failing();
        // Whatever step failed, the list is exactly the old one or exactly the new one, never a mixture.
        let listed = store.labels().unwrap();
        match result {
            Ok(()) => assert_eq!(listed, after, "step {n} (only clean-up failed)"),
            Err(_) => assert!(listed == before || listed == after, "step {n} left {} labels", listed.len()),
        }
        // Every label that was there and stays is still there, secret and all.
        for l in before.iter().filter(|l| after.contains(l)) {
            assert!(store.contains(l).unwrap(), "step {n} lost the secret {l:?}");
        }
        // And doing it again finishes the job.
        change(&store).unwrap();
        assert_eq!(store.labels().unwrap(), after, "step {n}: the retry");
    }
}

#[test]
fn a_set_that_fails_at_any_step_leaves_the_old_list_and_a_retry_finishes_it() {
    // A label that sorts before a full first shard pushes the last label of every shard into the next one.
    let before: Vec<_> = (0..40).map(long_label).collect();
    let first = label("0/first");
    let mut after = before.clone();
    after.insert(0, first.clone());
    check_every_failure_step(|store| store.set(&first, SecretValue::new("v")), &before, &after);
}

#[test]
fn a_delete_that_fails_at_any_step_leaves_a_list_that_a_retry_finishes() {
    let before: Vec<_> = (0..40).map(long_label).collect();
    let gone = before[0].clone();
    let after = before[1..].to_vec();
    check_every_failure_step(|store| store.delete(&gone), &before, &after);
}

#[test]
fn a_set_into_an_empty_store_that_fails_still_works_when_retried() {
    check_every_failure_step(|store| store.set(&label("a/b"), SecretValue::new("v")), &[], &[label("a/b")]);
}

#[test]
fn separate_handles_in_one_process_do_not_lose_each_others_labels() {
    let keychain = Arc::new(MapBackend { pause: Some(Duration::from_micros(200)), ..MapBackend::default() });
    let dir = tempfile::tempdir().unwrap();
    let handles: Vec<_> = (0..8).map(|_| Indexed::new(Arc::clone(&keychain), DirLock::new(dir.path()))).collect();
    thread::scope(|scope| {
        for (n, handle) in handles.iter().enumerate() {
            scope.spawn(move || {
                for i in 0..10 {
                    set_ok(handle, &label(&format!("t{n}/l{i}")));
                }
            });
        }
    });
    let listed = handles[0].labels().unwrap();
    assert_eq!(listed.len(), 8 * 10);
    assert!(listed.iter().all(|l| handles[1].contains(l).unwrap()));
}

/// The child half of `separate_processes_do_not_lose_each_others_labels`: a no-op unless the parent started it.
#[test]
fn two_process_child() {
    let Ok(job) = std::env::var("OMUZAKI_SECRETS_TEST_CHILD") else { return };
    let (keychain, rest) = job.split_once('|').unwrap();
    let (lock_dir, id) = rest.split_once('|').unwrap();
    let store = Indexed::new(DirBackend { dir: keychain.into() }, DirLock::new(Path::new(lock_dir)));
    for i in 0..10 {
        set_ok(&store, &label(&format!("p{id}/l{i}")));
    }
}

#[test]
fn separate_processes_do_not_lose_each_others_labels() {
    let keychain = tempfile::tempdir().unwrap();
    let lock_dir = tempfile::tempdir().unwrap();
    // The four run together, not one after another.
    let running: Vec<_> = (0..4)
        .map(|id| {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "store::tests::two_process_child", "--nocapture"])
                .env(
                    "OMUZAKI_SECRETS_TEST_CHILD",
                    format!("{}|{}|{id}", keychain.path().display(), lock_dir.path().display()),
                )
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in running {
        assert!(child.wait().unwrap().success());
    }
    let store = Indexed::new(DirBackend { dir: keychain.path().into() }, DirLock::new(lock_dir.path()));
    let listed = store.labels().unwrap();
    assert_eq!(listed.len(), 4 * 10, "{listed:?}");
    assert!(listed.iter().all(|l| store.contains(l).unwrap()));
}

/// Runs `op` on another thread while this one holds the lock: it must not finish, and must not touch the keychain,
/// until the lock is let go.
fn waits_for_the_lock(op: impl FnOnce(&Indexed<MapBackend>) + Send + 'static) {
    let store = Arc::new(fixture(MapBackend::default()));
    set_ok(&**store, &label("a/b"));
    let writes_before = store.backend.users();
    let held = DirLock::new(store.dir.path()).acquire().unwrap();
    let (done, finished) = mpsc::channel();
    let worker = Arc::clone(&store);
    let handle = thread::spawn(move || {
        op(&worker.store);
        done.send(()).unwrap();
    });
    assert!(finished.recv_timeout(Duration::from_millis(300)).is_err(), "it ran while the lock was held");
    assert_eq!(store.backend.users(), writes_before, "it wrote while the lock was held");
    drop(held);
    finished.recv_timeout(Duration::from_secs(10)).unwrap();
    handle.join().unwrap();
}

#[test]
fn labels_waits_for_the_lock() {
    waits_for_the_lock(|s| drop(s.labels().unwrap()));
}

#[test]
fn set_waits_for_the_lock_before_it_writes_the_secret() {
    waits_for_the_lock(|s| set_ok(s, &label("c/d")));
}

#[test]
fn delete_waits_for_the_lock_before_it_removes_the_secret() {
    waits_for_the_lock(|s| s.delete(&label("a/b")).unwrap());
}

#[test]
fn a_data_directory_that_cannot_exist_is_an_error_not_a_panic() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let store = Indexed::new(MapBackend::default(), DirLock::new(&file.path().join("inside-a-file")));
    assert!(matches!(store.labels(), Err(SecretError::Backend { .. })));
    assert!(matches!(store.set(&label("a/b"), SecretValue::new("v")), Err(SecretError::Backend { .. })));
    assert!(store.backend.users().is_empty(), "no secret is written without the lock");
}

#[test]
fn a_shard_is_never_wider_than_the_budget_whatever_is_stored() {
    let store = fixture(MapBackend::default());
    for i in 0..30 {
        set_ok(&*store, &long_label(i));
    }
    for user in store.backend.index_users().iter().filter(|u| u.starts_with("@labels.")) {
        let text = store.backend.get(user).unwrap().unwrap();
        assert!(text.expose().encode_utf16().count() * 2 <= SHARD_BUDGET, "{user}");
    }
}
