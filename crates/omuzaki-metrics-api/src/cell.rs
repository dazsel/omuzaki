//! The seqlock cell: one writer, any number of readers, safe Rust only.
//!
//! This is Boehm's seqlock ("Can Seqlocks Get Along with Programming Language Memory Models?", 2012) built from
//! atomics, so a reader that races the writer reads atomics and discards the result, never touching plain memory.
//!
//! It keeps **two slots** and writes them in turn, so a store never touches the value readers are reading: the newest
//! complete store stays readable for the whole of the next one. A single-slot seqlock makes every read that overlaps
//! a store wait for it, and a writer descheduled mid-store (or a slow debug build) can then outlast all of a read's
//! tries (#2250). Here a read is retried only when the writer finished one store and was well into the next one
//! during the read: two stores inside one read.

#[cfg(loom)]
use loom::sync::atomic::{AtomicU64, Ordering, fence};
#[cfg(loom)]
use loom::thread::yield_now;
#[cfg(not(loom))]
use std::sync::atomic::{AtomicU64, Ordering, fence};
#[cfg(not(loom))]
use std::thread::yield_now;

/// How many times a read tries before it gives the section up as `None`.
pub(crate) const MAX_TRIES: u32 = 16;

/// Tries a reader spins through before it starts giving up its time slice between tries.
const SPIN_TRIES: u32 = 4;

/// One of the two slots. `seq` is `2k` once store number `k` (from 1) is complete in it, `2k - 1` while store `k` is
/// being written into it, and `0` before its first store.
#[derive(Debug)]
struct Slot<const N: usize> {
    seq: AtomicU64,
    words: [AtomicU64; N],
}

/// A latest-value cell of `N` words. `latest` is the number of the newest complete store (`0` before the first);
/// store `k` lives in slot `k % 2`.
#[derive(Debug)]
pub(crate) struct SeqCell<const N: usize> {
    latest: AtomicU64,
    slots: [Slot<N>; 2],
}

impl<const N: usize> SeqCell<N> {
    /// An empty cell: [`load`](Self::load) gives `None` until the first store.
    pub(crate) fn new() -> Self {
        let slot = || Slot { seq: AtomicU64::new(0), words: std::array::from_fn(|_| AtomicU64::new(0)) };
        Self { latest: AtomicU64::new(0), slots: [slot(), slot()] }
    }

    /// Publishes `words`. Only one thread may call this at a time; the section writers are not `Clone` and publish
    /// through `&mut self`, which is how that is enforced. Never waits, never allocates.
    pub(crate) fn store(&self, words: &[u64; N]) {
        // Only the writer changes `latest`, so a relaxed read of its own last value is exact.
        let k = self.latest.load(Ordering::Relaxed).wrapping_add(1);
        let slot = &self.slots[slot_of(k)];
        // Release: a reader that sees this slot mid-write also sees store `k - 1` complete in the other one.
        slot.seq.store(k.wrapping_mul(2).wrapping_sub(1), Ordering::Release);
        // Orders the odd `seq` before every word store below, so a reader that sees a new word also sees `seq` change.
        fence(Ordering::Release);
        for (cell, word) in slot.words.iter().zip(words) {
            cell.store(*word, Ordering::Relaxed);
        }
        slot.seq.store(k.wrapping_mul(2), Ordering::Release);
        self.latest.store(k, Ordering::Release);
    }

    /// The latest stored words, or `None` before the first store or when [`MAX_TRIES`] reads were all torn.
    /// Never blocks the writer; a torn read is retried and never returned.
    pub(crate) fn load(&self) -> Option<[u64; N]> {
        self.load_counting(&mut 0)
    }

    /// [`load`](Self::load), adding the number of tries it made to `tries`.
    fn load_counting(&self, tries: &mut u32) -> Option<[u64; N]> {
        for _ in 0..MAX_TRIES {
            *tries += 1;
            let k = self.latest.load(Ordering::Acquire);
            if k == 0 {
                return None;
            }
            let slot = &self.slots[slot_of(k)];
            // Exactly store `k`, not a later one that has since reused the slot: a read returns the store `latest`
            // named, so a later read (which sees `latest` no lower) never returns an older one.
            let want = k.wrapping_mul(2);
            if slot.seq.load(Ordering::Acquire) != want {
                // The writer has lapped this read: two stores since `latest` was read. Let it run before retrying.
                if *tries > SPIN_TRIES {
                    yield_now();
                } else {
                    std::hint::spin_loop();
                }
                continue;
            }
            let mut out = [0u64; N];
            for (word, cell) in out.iter_mut().zip(&slot.words) {
                *word = cell.load(Ordering::Relaxed);
            }
            // Orders the word loads above before the second `seq` read.
            fence(Ordering::Acquire);
            if slot.seq.load(Ordering::Relaxed) == want {
                return Some(out);
            }
        }
        None
    }
}

/// The slot store `k` is written to.
fn slot_of(k: u64) -> usize {
    usize::from(k % 2 == 1)
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn empty_until_first_store() {
        let cell = SeqCell::<3>::new();
        assert_eq!(cell.load(), None);
        cell.store(&[1, 2, 3]);
        assert_eq!(cell.load(), Some([1, 2, 3]));
        cell.store(&[4, 5, 6]);
        assert_eq!(cell.load(), Some([4, 5, 6]));
    }

    #[test]
    fn a_read_during_a_stalled_store_gets_the_last_complete_value_at_once() {
        let cell = SeqCell::<2>::new();
        cell.store(&[7, 8]);
        // The writer is descheduled halfway through the next store: its slot is odd, half its words are new.
        let next = &cell.slots[slot_of(2)];
        next.seq.store(3, Ordering::Release);
        next.words[0].store(9, Ordering::Relaxed);
        let mut tries = 0;
        assert_eq!(cell.load_counting(&mut tries), Some([7, 8]));
        assert_eq!(tries, 1, "a stalled writer costs a read nothing");
        // The writer finishes: the next read sees the new value on its first try.
        next.words[1].store(10, Ordering::Relaxed);
        next.seq.store(4, Ordering::Release);
        cell.latest.store(2, Ordering::Release);
        tries = 0;
        assert_eq!(cell.load_counting(&mut tries), Some([9, 10]));
        assert_eq!(tries, 1);
    }

    #[test]
    fn a_read_the_writer_keeps_lapping_gives_up_after_sixteen_tries() {
        let cell = SeqCell::<2>::new();
        cell.store(&[1, 1]);
        // `latest` names store 1, but its slot already holds store 3 being written: the writer lapped the read.
        cell.slots[slot_of(1)].seq.store(5, Ordering::Release);
        let mut tries = 0;
        assert_eq!(cell.load_counting(&mut tries), None);
        assert_eq!(tries, MAX_TRIES);
    }

    #[test]
    fn stores_alternate_slots_and_each_read_returns_the_newest() {
        let cell = SeqCell::<1>::new();
        for k in 1..=5_u64 {
            cell.store(&[k * 10]);
            assert_eq!(cell.load(), Some([k * 10]));
            assert_eq!(cell.slots[slot_of(k)].seq.load(Ordering::Relaxed), 2 * k);
        }
    }
}

#[cfg(all(test, loom))]
mod loom_tests {
    use super::*;
    use loom::sync::Arc;
    use loom::thread;

    /// Words all hold the same number, so any mix of two stores is visible as unequal words.
    fn check(words: [u64; 2]) -> u64 {
        assert_eq!(words[0], words[1], "torn read");
        words[0]
    }

    /// Two publishes racing two reads: a read is never torn (the second publish reuses the first value's slot) and the
    /// second read never sees an older value than the first.
    #[test]
    fn two_publishes_against_two_reads_are_never_torn_or_backwards() {
        // Three preemptions find the interleavings that matter here; unbounded, the retry loop makes the model take minutes.
        let mut builder = loom::model::Builder::new();
        builder.preemption_bound = Some(3);
        builder.check(|| {
            let cell = Arc::new(SeqCell::<2>::new());
            cell.store(&[1, 1]);
            let writer = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || {
                    cell.store(&[2, 2]);
                    // A second store reuses the slot store 1 is in: a read that saw `latest = 1` must not mix them.
                    cell.store(&[3, 3]);
                })
            };
            let reader = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || {
                    let first = cell.load().map(check);
                    let second = cell.load().map(check);
                    (first, second)
                })
            };
            let (first, second) = reader.join().expect("reader");
            writer.join().expect("writer");
            if let (Some(a), Some(b)) = (first, second) {
                assert!(a <= b, "went backwards: {a} then {b}");
            }
            // After the writer is done the latest value is always readable.
            assert_eq!(cell.load().map(check), Some(3));
        });
    }
}
