//! A missing fact's default belongs to one `Published` and one fact: a `Merged` with interior mutability can't carry a
//! change from one build into another, or into another fact with the same merged type.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use omuzaki_char_api::{Fact, PartId, Published, Publishes};

/// A merged value with interior mutability: `Clone + Default + Send + Sync`, and shared by every clone.
type Shared = Arc<Mutex<Vec<u32>>>;

macro_rules! shared_fact {
    ($name:ident, $label:literal) => {
        struct $name;
        impl Fact for $name {
            const NAME: &'static str = $label;
            type Value = u32;
            type Merged = Shared;
            fn merge(merged: &mut Shared, _part: &PartId, value: u32) -> Result<(), String> {
                merged.lock().unwrap().push(value);
                Ok(())
            }
        }
    };
}
shared_fact!(One, "test-default-one");
shared_fact!(Two, "test-default-two");

fn contents(m: &Shared) -> Vec<u32> {
    m.lock().unwrap().clone()
}

#[test]
fn a_change_through_one_published_is_not_seen_by_another_or_by_another_fact() {
    let (a, b) = (Published::default(), Published::default());
    a.get::<One>().lock().unwrap().push(7);
    assert_eq!(contents(a.get::<One>()), [7], "the view that was changed keeps the change");
    assert!(contents(b.get::<One>()).is_empty(), "another Published still sees the empty value");
    assert!(contents(a.get::<Two>()).is_empty(), "another fact with the same merged type does too");
    assert_eq!(contents(Published::default().get::<One>()), Vec::<u32>::new());
}

#[test]
fn a_read_returns_the_same_value_every_time() {
    let a = Published::default();
    assert!(std::ptr::eq(a.get::<One>(), a.get::<One>()), "made once per view, not per read");
    assert!(!std::ptr::eq(a.get::<One>(), a.get::<Two>()));
}

#[test]
fn a_clone_after_a_read_does_not_share_the_default() {
    let a = Published::default();
    a.get::<One>().lock().unwrap().push(1);
    let b = a.clone();
    assert!(contents(b.get::<One>()).is_empty(), "the clone starts with a fresh default");
    b.get::<One>().lock().unwrap().push(2);
    assert_eq!(contents(a.get::<One>()), [1]);
    assert_eq!(contents(b.get::<One>()), [2]);
}

#[test]
fn a_published_fact_never_uses_a_default() {
    let mut a = Published::default();
    a.get::<One>().lock().unwrap().push(9);
    a.absorb(&PartId::new("p").unwrap(), Publishes::default().with::<One>(3)).unwrap();
    assert_eq!(contents(a.get::<One>()), [3], "the published value replaces the default for readers");
    assert_eq!(contents(Published::default().get::<One>()), Vec::<u32>::new());
}

#[test]
fn views_are_independent_across_threads() {
    let a = Published::default();
    let a = &a;
    std::thread::scope(|s| {
        for i in 0..8 {
            s.spawn(move || {
                let p = Published::default();
                p.get::<One>().lock().unwrap().push(i);
                assert_eq!(contents(p.get::<One>()), [i]);
                let _ = a.get::<Two>();
            });
        }
    });
    assert_eq!(contents(a.get::<One>()), Vec::<u32>::new());
}

// ---- a Default that reads the view it is made for

static VIEW: OnceLock<Published> = OnceLock::new();

shared_fact!(Inner, "test-default-inner");

/// A merged value whose `Default` reads another missing fact from the same `Published`.
#[derive(Clone)]
struct Reader(usize);

impl Default for Reader {
    fn default() -> Self {
        Self(VIEW.get().map_or(usize::MAX, |view| view.get::<Inner>().lock().unwrap().len()))
    }
}

struct Outer;
impl Fact for Outer {
    const NAME: &'static str = "test-default-outer";
    type Value = u32;
    type Merged = Reader;
    fn merge(_merged: &mut Reader, _part: &PartId, _value: u32) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn a_default_that_reads_a_missing_fact_of_its_own_view_does_not_hang() {
    let view = VIEW.get_or_init(Published::default);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(view.get::<Outer>().0);
    });
    let seen = rx.recv_timeout(Duration::from_secs(5)).expect("the read finished instead of hanging");
    assert_eq!(seen, 0, "the default read the empty value of the other fact");
    assert_eq!(view.get::<Outer>().0, 0);
    assert!(std::ptr::eq(view.get::<Inner>(), view.get::<Inner>()));
}
