//! `ProjectLock`: one writer per project folder, held by an OS lock.

// Helpers outside `#[test]` functions may unwrap too: a failed setup should just panic.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use omuzaki_doc::{ProjectLock, StoreError};
use tempfile::TempDir;

/// Set in the environment of the child process, which takes the lock and then waits to be killed.
const HOLDER_DIR_ENV: &str = "OMUZAKI_DOC_LOCK_TEST_HOLDER_DIR";
/// Printed by the child once it holds the lock.
const HELD_MARKER: &str = "LOCK-HELD-BY-CHILD";
/// The child gives up after this long even if nothing kills it, so a leaked child can't run forever.
const HOLDER_BACKSTOP: Duration = Duration::from_secs(120);

/// Not a real test: the body runs only in the child process started by the test below.
#[test]
fn lock_holder_child() {
    let Some(dir) = std::env::var_os(HOLDER_DIR_ENV) else {
        return;
    };
    let _lock = ProjectLock::acquire(dir).unwrap();
    println!("{HELD_MARKER}");
    std::thread::sleep(HOLDER_BACKSTOP);
}

/// Held while a test spawns a child and while a test drops a lock and takes it again.
///
/// A forked child shares every open file description of this process until it execs, and an `flock` lasts as long as
/// any copy of its description does. So a spawn on one test thread can keep another test's just-dropped lock held for
/// a moment, and that test's re-acquire then fails with `Locked` naming this process.
static FORK_VS_RELEASE: Mutex<()> = Mutex::new(());

/// The holder child, killed and reaped when dropped, so a failing assertion can't leak it.
struct Holder(Child);

impl Drop for Holder {
    fn drop(&mut self) {
        // The child may already be dead (the test killed it on purpose); either way it must be reaped.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts a child that takes the lock in `dir` and reports it; returns once the child holds the lock.
fn start_holder(dir: &Path) -> Holder {
    let spawn_guard = FORK_VS_RELEASE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_holder_child", "--nocapture", "--test-threads=1"])
        .env(HOLDER_DIR_ENV, dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // `spawn` returns after the exec, which has closed the inherited descriptors.
    drop(spawn_guard);
    let stdout = child.stdout.take().unwrap();
    let holder = Holder(child);
    wait_for_marker(stdout);
    holder
}

/// Reads the child's output until it reports holding the lock; panics if the child exits first.
fn wait_for_marker(stdout: ChildStdout) {
    let mut lines = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(lines.read_line(&mut line).unwrap() > 0, "the child exited without taking the lock");
        if line.contains(HELD_MARKER) {
            return;
        }
    }
}

#[test]
fn a_second_process_is_refused_naming_the_first_and_is_freed_by_its_death() {
    let tmp = TempDir::new().unwrap();
    let mut holder = start_holder(tmp.path());

    match ProjectLock::acquire(tmp.path()) {
        Err(StoreError::Locked { pid, host, since }) => {
            assert_eq!(pid, holder.0.id());
            assert_ne!(host, "");
            assert!(since > 0);
        }
        other => panic!("expected Locked while the child holds the lock, got {other:?}"),
    }

    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    let _lock = ProjectLock::acquire(tmp.path()).unwrap();
}

#[test]
fn a_second_handle_in_this_process_is_refused_with_this_pid() {
    let tmp = TempDir::new().unwrap();
    let _first = ProjectLock::acquire(tmp.path()).unwrap();
    match ProjectLock::acquire(tmp.path()) {
        Err(StoreError::Locked { pid, .. }) => assert_eq!(pid, std::process::id()),
        other => panic!("expected Locked, got {other:?}"),
    }
}

#[test]
fn a_holder_with_no_owner_file_is_refused_as_unknown() {
    let tmp = TempDir::new().unwrap();
    let _first = ProjectLock::acquire(tmp.path()).unwrap();
    fs::remove_file(tmp.path().join(".omuzaki").join("lock.owner")).unwrap();
    match ProjectLock::acquire(tmp.path()) {
        Err(StoreError::Locked { pid, host, since }) => {
            assert_eq!((pid, host.as_str(), since), (0, "unknown", 0));
        }
        other => panic!("expected Locked, got {other:?}"),
    }
}

#[test]
fn the_message_names_a_known_holder_and_stays_plain_when_unknown() {
    let known = StoreError::Locked { pid: 42, host: "box".to_owned(), since: 1 };
    assert_eq!(known.to_string(), "the project is open in another Omuzaki (pid 42 on box); close that one first");
    let unknown = StoreError::Locked { pid: 0, host: "unknown".to_owned(), since: 0 };
    assert_eq!(unknown.to_string(), "the project is open in another Omuzaki; close that one first");
}

#[test]
fn dropping_the_lock_frees_the_project() {
    let tmp = TempDir::new().unwrap();
    let _no_fork_meanwhile = FORK_VS_RELEASE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let lock = ProjectLock::acquire(tmp.path()).unwrap();
    drop(lock);
    drop(ProjectLock::acquire(tmp.path()).unwrap());
}

#[test]
fn the_owner_file_records_this_process_and_the_lock_file_is_not_written() {
    let tmp = TempDir::new().unwrap();
    let _lock = ProjectLock::acquire(tmp.path()).unwrap();
    let owner = fs::read_to_string(tmp.path().join(".omuzaki").join("lock.owner")).unwrap();
    assert!(owner.contains(&format!("pid = {}\n", std::process::id())), "{owner}");
    assert!(owner.contains("host = "), "{owner}");
    assert!(owner.contains("since = "), "{owner}");
    assert_eq!(fs::metadata(tmp.path().join(".omuzaki").join("lock")).unwrap().len(), 0);
}

#[test]
fn a_dead_holders_owner_text_does_not_block_and_is_replaced() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".omuzaki")).unwrap();
    fs::write(tmp.path().join(".omuzaki").join("lock.owner"), "pid = 4242\nhost = gone\nsince = 1\n").unwrap();
    let _lock = ProjectLock::acquire(tmp.path()).unwrap();
    let text = fs::read_to_string(tmp.path().join(".omuzaki").join("lock.owner")).unwrap();
    assert!(!text.contains("host = gone"), "the new holder replaces the old text: {text}");
}
