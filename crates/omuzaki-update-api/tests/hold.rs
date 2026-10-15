//! `Hold` is an RAII token: dropping releases once, `none` holds nothing, and a leaked hold is never released.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_update_api::hold::Hold;

fn counted() -> (Arc<AtomicUsize>, Hold) {
    let released = Arc::new(AtomicUsize::new(0));
    let r = Arc::clone(&released);
    (released, Hold::new(move || _ = r.fetch_add(1, Ordering::SeqCst)))
}

#[test]
fn dropping_releases_exactly_once() {
    let (released, hold) = counted();
    assert_eq!(released.load(Ordering::SeqCst), 0);
    drop(hold);
    assert_eq!(released.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_one_hold_leaves_the_other() {
    let (a, first) = counted();
    let (b, second) = counted();
    drop(first);
    assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (1, 0));
    drop(second);
    assert_eq!(b.load(Ordering::SeqCst), 1);
}

#[test]
fn a_leaked_hold_is_not_released() {
    let (released, hold) = counted();
    std::mem::forget(hold);
    assert_eq!(released.load(Ordering::SeqCst), 0);
}

#[test]
fn none_holds_nothing() {
    let hold = Hold::none();
    assert!(format!("{hold:?}").contains("false"));
    drop(hold);
}
