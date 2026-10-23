//! Facts at scale: two facts that share a name never share a slot, a default that reads another fact doesn't deadlock,
//! and a part that publishes nothing copies nothing.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use omuzaki_char_api::{Agree, BuildError, Fact, Last, PartId, Published, Publishes};

fn pid(name: &str) -> PartId {
    PartId::new(name).unwrap()
}

// ---- two facts, one name

omuzaki_char_api::fact! {
    /// Takes the last value.
    TakesLast = "test-clash", Last<u32>;
}
omuzaki_char_api::fact! {
    /// Must agree, under the same name and with the same payload type as `TakesLast`.
    MustAgree = "test-clash", Agree<u32>;
}
omuzaki_char_api::register_fact!(TakesLast);
omuzaki_char_api::register_fact!(MustAgree);

fn names_both(err: &BuildError, part: &str) {
    let BuildError::Part { part: who, message } = err else { panic!("a part error, got {err:?}") };
    assert_eq!(who.as_str(), part);
    assert!(message.contains("two different facts are called 'test-clash'"), "{message}");
    assert!(message.contains("TakesLast") && message.contains("MustAgree"), "names both facts: {message}");
}

#[test]
fn two_facts_with_one_name_in_one_part_are_a_part_error_naming_both() {
    let p = Publishes::default().with::<TakesLast>(1).with::<MustAgree>(2);
    assert_eq!(p.values::<TakesLast>().copied().collect::<Vec<_>>(), [1]);
    assert_eq!(p.values::<MustAgree>().count(), 0, "a fact never reads another fact's values");
    let mut published = Published::default();
    names_both(&published.absorb(&pid("a"), p).unwrap_err(), "a");
    assert_eq!(*published.get::<TakesLast>(), None, "nothing is absorbed");
}

#[test]
fn a_later_part_whose_fact_reuses_a_name_is_a_part_error_naming_both() {
    let mut published = Published::default();
    published.absorb(&pid("a"), Publishes::default().with::<TakesLast>(1)).unwrap();
    // Before identities were checked, this merged by `Last` and replaced 1 without an agreement check.
    let err = published.absorb(&pid("b"), Publishes::default().with::<MustAgree>(2)).unwrap_err();
    names_both(&err, "b");
    assert_eq!(*published.get::<TakesLast>(), Some(1), "the earlier fact is kept");
    assert_eq!(*published.get::<MustAgree>(), None, "a fact never reads another fact's merged value");
}

#[test]
fn registering_two_facts_with_one_name_is_an_error_naming_both() {
    let message = omuzaki_char_api::facts().err().expect("the registry finds the shared name");
    assert!(message.contains("two different facts are called 'test-clash'"), "{message}");
    assert!(message.contains("TakesLast") && message.contains("MustAgree"), "names both facts: {message}");
}

// ---- a default that reads another fact

/// The merged value of `Inner`: its own type, so its default is made by this test only.
#[derive(Clone, Debug, Default, PartialEq)]
struct InnerValue(u32);

omuzaki_char_api::fact! {
    /// Read by `Outer`'s default, and never published.
    InnerFact = "test-inner", Last<InnerValue>;
}

/// A merged value whose default reads another fact that nothing published.
#[derive(Clone, Debug, PartialEq)]
struct Outer(Option<InnerValue>);

impl Default for Outer {
    fn default() -> Self {
        Self(Published::default().get::<InnerFact>().clone())
    }
}

struct OuterFact;

impl Fact for OuterFact {
    const NAME: &'static str = "test-outer";
    type Value = u32;
    type Merged = Outer;

    fn merge(_: &mut Outer, _: &PartId, _: u32) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn a_default_that_reads_another_missing_fact_finishes() {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let outer = Published::default().get::<OuterFact>().clone();
        send.send(outer).unwrap();
    });
    // A deadlock leaves the thread blocked and fails here instead of hanging the test run.
    let outer = receive.recv_timeout(Duration::from_secs(60)).expect("reading the default finished");
    assert_eq!(outer, Outer(None));
}

// ---- a silent part copies nothing

static CLONES: AtomicUsize = AtomicUsize::new(0);

/// A merged value that counts its clones.
#[derive(Default)]
struct Counted(Vec<u32>);

impl Clone for Counted {
    fn clone(&self) -> Self {
        CLONES.fetch_add(1, Ordering::SeqCst);
        Self(self.0.clone())
    }
}

struct CountedFact;

impl Fact for CountedFact {
    const NAME: &'static str = "test-counted";
    type Value = u32;
    type Merged = Counted;

    fn merge(merged: &mut Counted, _: &PartId, value: u32) -> Result<(), String> {
        merged.0.push(value);
        Ok(())
    }
}

/// The clones of [`Tallied`], counted apart from [`CLONES`] so the two tests can run at once.
static TALLIED_CLONES: AtomicUsize = AtomicUsize::new(0);

/// A merged value that counts its clones in [`TALLIED_CLONES`].
#[derive(Default)]
struct Tallied(Vec<u32>);

impl Clone for Tallied {
    fn clone(&self) -> Self {
        TALLIED_CLONES.fetch_add(1, Ordering::SeqCst);
        Self(self.0.clone())
    }
}

struct TalliedFact;

impl Fact for TalliedFact {
    const NAME: &'static str = "test-tallied";
    type Value = u32;
    type Merged = Tallied;

    fn merge(merged: &mut Tallied, _: &PartId, value: u32) -> Result<(), String> {
        merged.0.push(value);
        Ok(())
    }
}

/// A second fact, unrelated to [`TalliedFact`].
struct OtherFact;

impl Fact for OtherFact {
    const NAME: &'static str = "test-other";
    type Value = u32;
    type Merged = Vec<u32>;

    fn merge(merged: &mut Vec<u32>, _: &PartId, value: u32) -> Result<(), String> {
        merged.push(value);
        Ok(())
    }
}

/// #1579: absorbing a part stages only the facts it publishes, so a part that publishes another fact copies no merged
/// value of [`TalliedFact`], however many parts published it before.
#[test]
fn absorbing_a_part_clones_only_the_facts_it_publishes() {
    let mut published = Published::default();
    for (i, part) in ["a", "b", "c"].into_iter().enumerate() {
        published.absorb(&pid(part), Publishes::default().with::<TalliedFact>(u32::try_from(i).unwrap())).unwrap();
    }
    let before = TALLIED_CLONES.load(Ordering::SeqCst);
    for (i, part) in ["d", "e", "f"].into_iter().enumerate() {
        published.absorb(&pid(part), Publishes::default().with::<OtherFact>(u32::try_from(i).unwrap())).unwrap();
    }
    assert_eq!(TALLIED_CLONES.load(Ordering::SeqCst), before, "a part publishing another fact copies no tallied value");
    published.absorb(&pid("g"), Publishes::default().with::<TalliedFact>(3)).unwrap();
    assert_eq!(TALLIED_CLONES.load(Ordering::SeqCst), before + 1, "a part publishing it copies it once");
    assert_eq!(published.get::<TalliedFact>().0, [0, 1, 2, 3]);
    assert_eq!(*published.get::<OtherFact>(), [0, 1, 2]);
}

#[test]
fn absorbing_a_part_that_publishes_nothing_clones_nothing() {
    let mut published = Published::default();
    published.absorb(&pid("a"), Publishes::default().with::<CountedFact>(1)).unwrap();
    let before = CLONES.load(Ordering::SeqCst);
    for part in ["b", "c", "d"] {
        published.absorb(&pid(part), Publishes::default()).unwrap();
    }
    assert_eq!(CLONES.load(Ordering::SeqCst), before, "a silent part copies no merged fact");
    published.absorb(&pid("e"), Publishes::default().with::<CountedFact>(2)).unwrap();
    assert!(CLONES.load(Ordering::SeqCst) > before, "the counter does see the copy a publishing part makes");
    assert_eq!(published.get::<CountedFact>().0, [1, 2]);
}
