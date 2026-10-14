//! `CancelFlag` shares one flag between clones and threads; `NeverCancel` never fires; both work as `&dyn Cancel`.
#![allow(clippy::expect_used)] // tests may panic on a broken fixture

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use omuzaki_git_api::{Cancel, CancelFlag, NeverCancel};

#[test]
fn a_fresh_flag_is_not_cancelled() {
    assert!(!CancelFlag::new().is_cancelled());
    assert!(!CancelFlag::default().is_cancelled());
}

#[test]
fn clones_share_one_flag() {
    let a = CancelFlag::new();
    let b = a.clone();
    b.cancel();
    assert!(a.is_cancelled());
    assert!(b.is_cancelled());
}

#[test]
fn never_cancel_is_never_cancelled() {
    assert!(!NeverCancel.is_cancelled());
}

#[test]
fn both_handles_work_as_dyn_cancel() {
    let flag = CancelFlag::new();
    let handles: [&dyn Cancel; 2] = [&NeverCancel, &flag];
    assert!(handles.iter().all(|h| !h.is_cancelled()));
    flag.cancel();
    assert!(!handles[0].is_cancelled());
    assert!(handles[1].is_cancelled());
}

#[test]
fn a_cancel_on_one_thread_is_seen_on_another() {
    let flag = CancelFlag::new();
    let watcher = flag.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        ready_tx.send(()).expect("main is waiting");
        let start = Instant::now();
        while !watcher.is_cancelled() {
            if start.elapsed() > Duration::from_secs(5) {
                return false;
            }
            thread::yield_now();
        }
        true
    });
    ready_rx.recv().expect("watcher started");
    flag.cancel();
    assert!(handle.join().expect("watcher thread"), "the watcher never saw the cancel");
}
