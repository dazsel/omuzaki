//! The seqlock cell: one writer, any number of readers, safe Rust only.
//!
//! This is Boehm's seqlock ("Can Seqlocks Get Along with Programming Language Memory Models?", 2012) built from
//! atomics, so a reader that races the writer reads atomics and discards the result, never touching plain memory.

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

/// A latest-value cell of `N` words. `seq` is even when the words are stable (`0` before the first store) and odd
/// while the writer is inside [`store`](Self::store).
#[derive(Debug)]
pub(crate) struct SeqCell<const N: usize> {
    seq: AtomicU64,
    words: [AtomicU64; N],
}

impl<const N: usize> SeqCell<N> {
    /// An empty cell: [`load`](Self::load) gives `None` until the first store.
    pub(crate) fn new() -> Self {
        Self { seq: AtomicU64::new(0), words: std::array::from_fn(|_| AtomicU64::new(0)) }
    }

    /// Publishes `words`. Only one thread may call this at a time; the section writers are not `Clone` and publish
    /// through `&mut self`, which is how that is enforced. Never waits, never allocates.
    pub(crate) fn store(&self, words: &[u64; N]) {
        // Only the writer changes `seq`, so a relaxed read of its own last value is exact.
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        // Orders the odd `seq` before every word store below, so a reader that sees a new word also sees `seq` change.
        fence(Ordering::Release);
        for (cell, word) in self.words.iter().zip(words) {
            cell.store(*word, Ordering::Relaxed);
        }
        self.seq.store(s.wrapping_add(2), Ordering::Release);
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
            let before = self.seq.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if before % 2 == 1 {
                // The writer is mid-store, or was descheduled there: spinning through all the tries in a microsecond
                // would give up on a store that is about to finish, so after a few spins let it have the core. This
                // only ever delays the reader.
                if *tries > SPIN_TRIES {
                    yield_now();
                } else {
                    std::hint::spin_loop();
                }
                continue;
            }
            let mut out = [0u64; N];
            for (word, cell) in out.iter_mut().zip(&self.words) {
                *word = cell.load(Ordering::Relaxed);
            }
            // Orders the word loads above before the second `seq` read.
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return Some(out);
            }
        }
        None
    }
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
    fn a_read_inside_a_write_gives_up_after_sixteen_tries() {
        let cell = SeqCell::<2>::new();
        cell.store(&[7, 8]);
        // A writer stalled mid-store: `seq` stays odd.
        let stable = cell.seq.load(Ordering::Relaxed);
        cell.seq.store(stable + 1, Ordering::Relaxed);
        let mut tries = 0;
        assert_eq!(cell.load_counting(&mut tries), None);
        assert_eq!(tries, MAX_TRIES);
        // The writer finishes: the next read succeeds on its first try.
        cell.words[0].store(9, Ordering::Relaxed);
        cell.words[1].store(10, Ordering::Relaxed);
        cell.seq.store(stable + 2, Ordering::Release);
        tries = 0;
        assert_eq!(cell.load_counting(&mut tries), Some([9, 10]));
        assert_eq!(tries, 1);
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

    /// One publish racing two reads (and a second publish after the first): a read is never torn and the second
    /// read never sees an older value than the first.
    #[test]
    fn one_publish_against_two_reads_is_never_torn_or_backwards() {
        // Three preemptions find the interleavings that matter here; unbounded, the retry loop makes the model take minutes.
        let mut builder = loom::model::Builder::new();
        builder.preemption_bound = Some(3);
        builder.check(|| {
            let cell = Arc::new(SeqCell::<2>::new());
            cell.store(&[1, 1]);
            let writer = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.store(&[2, 2]))
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
            assert_eq!(cell.load().map(check), Some(2));
        });
    }
}
