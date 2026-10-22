//! The rings between a capture thread and a consumer (design §2): one producer and one consumer each, fixed size, no
//! lock and no allocation. An entry is a fixed number of `u64` words held in atomics, so the crate needs no `unsafe`
//! (the pattern of `omuzaki-audio-api`'s ring and `omuzaki-track-api`'s mailbox).
//!
//! The producer writes an entry's words, then publishes it by moving `tail` (release); the consumer reads up to
//! `tail` (acquire) and frees entries by moving `head` (release), which the producer reads (acquire) before reusing a
//! slot. A full ring refuses the push: the producer keeps the entry in its own backlog and applies the drop policy
//! there ([`Backlog`]), so nothing the consumer may be reading is ever touched.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Records the one thread allowed to write a ring or a snapshot, and fails a debug build if a second one writes.
#[derive(Debug, Default)]
pub(crate) struct WriterCheck {
    #[cfg(debug_assertions)]
    writer: AtomicU64,
}

impl WriterCheck {
    /// The calling thread is about to write.
    #[inline]
    pub(crate) fn check(&self) {
        #[cfg(debug_assertions)]
        {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::thread::current().id().hash(&mut hasher);
            // 0 means "no writer yet", so a thread hashing to 0 counts as 1.
            let me = hasher.finish().max(1);
            match self.writer.compare_exchange(0, me, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => {}
                Err(writer) => assert!(writer == me, "a second thread wrote a single-writer ring or snapshot"),
            }
        }
    }

    /// Forgets the writer: a backend that starts again may run on a new thread.
    pub(crate) fn release(&self) {
        #[cfg(debug_assertions)]
        self.writer.store(0, Ordering::Relaxed);
    }
}

/// A single-producer, single-consumer ring of entries of `W` words.
#[derive(Debug)]
pub(crate) struct Ring<const W: usize> {
    slots: Box<[[AtomicU64; W]]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    /// Set by the producer whenever its backlog dropped an entry; the consumer takes it to reconcile.
    dropped: AtomicBool,
    writer: WriterCheck,
}

impl<const W: usize> Ring<W> {
    /// A ring of `capacity` entries.
    pub(crate) fn new(capacity: usize) -> Self {
        let slots = (0..capacity.max(1)).map(|_| std::array::from_fn(|_| AtomicU64::new(0))).collect();
        Self {
            slots,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicBool::new(false),
            writer: WriterCheck::default(),
        }
    }

    /// The producer: appends `words`; `false` when the ring is full.
    pub(crate) fn push(&self, words: [u64; W]) -> bool {
        self.writer.check();
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= self.slots.len() {
            return false;
        }
        if let Some(slot) = self.slots.get(tail % self.slots.len()) {
            for (cell, word) in slot.iter().zip(words) {
                cell.store(word, Ordering::Relaxed);
            }
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// The consumer: the oldest entry, left in the ring.
    pub(crate) fn peek(&self) -> Option<[u64; W]> {
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }
        let slot = self.slots.get(head % self.slots.len())?;
        Some(std::array::from_fn(|i| slot.get(i).map_or(0, |cell| cell.load(Ordering::Relaxed))))
    }

    /// The consumer: removes the oldest entry.
    pub(crate) fn pop(&self) {
        let head = self.head.load(Ordering::Relaxed);
        if head != self.tail.load(Ordering::Acquire) {
            self.head.store(head.wrapping_add(1), Ordering::Release);
        }
    }

    /// The producer: notes that its backlog dropped an entry.
    pub(crate) fn mark_dropped(&self) {
        self.dropped.store(true, Ordering::Release);
    }

    /// The consumer: whether anything was dropped since it last asked.
    pub(crate) fn take_dropped(&self) -> bool {
        self.dropped.swap(false, Ordering::AcqRel)
    }

    /// A new producer thread takes over (a backend that started again).
    pub(crate) fn new_writer(&self) {
        self.writer.release();
    }
}

/// How the backlog may drop an entry when it is full: what kind of entry it is, so releases are kept.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Keep {
    /// Dropped first: a pointer or pen sample, a binding repeat.
    Cheap,
    /// A press or key down: dropped with its release as a pair if one is queued, else alone.
    Press {
        /// What pairs it with its release: the source and the zone or binding.
        pair: u32,
    },
    /// A release or key up: dropped only when nothing else is left.
    Release {
        /// The pair key of the press it ends.
        pair: u32,
    },
    /// Never dropped while anything else can be (devices coming and going).
    Always,
}

/// The producer's own queue in front of a ring: entries wait here while the ring is full, and the drop policy runs
/// here, where nothing is shared. Fixed capacity, allocated once.
#[derive(Debug)]
pub(crate) struct Backlog<const W: usize> {
    entries: VecDeque<([u64; W], Keep)>,
    capacity: usize,
    /// Press pairs whose press was dropped alone: their later release is dropped too.
    orphaned: Vec<u32>,
}

impl<const W: usize> Backlog<W> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self { entries: VecDeque::with_capacity(capacity), capacity, orphaned: Vec::with_capacity(capacity) }
    }

    /// Moves what fits into `ring`, oldest first.
    pub(crate) fn flush(&mut self, ring: &Ring<W>) {
        while let Some((words, _)) = self.entries.front() {
            if !ring.push(*words) {
                break;
            }
            self.entries.pop_front();
        }
    }

    /// Queues an entry behind the backlog (or straight into `ring` when nothing waits), dropping by the policy when
    /// the backlog is full.
    pub(crate) fn push(&mut self, ring: &Ring<W>, words: [u64; W], keep: Keep) {
        if let Keep::Release { pair } = keep
            && let Some(at) = self.orphaned.iter().position(|p| *p == pair)
        {
            // Its press was dropped: the release would end nothing.
            self.orphaned.swap_remove(at);
            return;
        }
        self.flush(ring);
        if self.entries.is_empty() && ring.push(words) {
            return;
        }
        if self.entries.len() >= self.capacity {
            self.drop_one();
            ring.mark_dropped();
            if let Keep::Release { pair } = keep
                && let Some(at) = self.orphaned.iter().position(|p| *p == pair)
            {
                // The drop took this release's own press.
                self.orphaned.swap_remove(at);
                return;
            }
        }
        self.entries.push_back((words, keep));
    }

    /// Discards everything waiting (a pause or a stop: nothing read before it may reach a consumer).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.orphaned.clear();
    }

    /// Drops one entry: the oldest cheap one, else the oldest press with its queued release, else the oldest lone
    /// press (its release is then dropped when it comes), else the oldest entry.
    fn drop_one(&mut self) {
        if let Some(at) = self.entries.iter().position(|(_, k)| *k == Keep::Cheap) {
            self.entries.remove(at);
            return;
        }
        let pair = self.entries.iter().enumerate().find_map(|(i, (_, k))| match k {
            Keep::Press { pair } => self
                .entries
                .iter()
                .skip(i + 1)
                .position(|(_, k)| *k == Keep::Release { pair: *pair })
                .map(|j| (i, i + 1 + j)),
            _ => None,
        });
        if let Some((press, release)) = pair {
            self.entries.remove(release);
            self.entries.remove(press);
            return;
        }
        if let Some(at) = self.entries.iter().position(|(_, k)| matches!(k, Keep::Press { .. })) {
            if let Some((_, Keep::Press { pair })) = self.entries.remove(at)
                && self.orphaned.len() < self.capacity
            {
                self.orphaned.push(pair);
            }
            return;
        }
        self.entries.pop_front();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_is_first_in_first_out_and_refuses_when_full() {
        let ring = Ring::<2>::new(3);
        for i in 0..3 {
            assert!(ring.push([i, i * 10]));
        }
        assert!(!ring.push([9, 9]));
        assert_eq!(ring.peek(), Some([0, 0]));
        ring.pop();
        assert!(ring.push([3, 30]));
        let drained: Vec<u64> = std::iter::from_fn(|| ring.peek().inspect(|_| ring.pop())).map(|w| w[0]).collect();
        assert_eq!(drained, [1, 2, 3]);
    }

    #[test]
    fn the_backlog_drops_cheap_entries_then_pairs_then_lone_presses_and_keeps_releases() {
        let ring = Ring::<1>::new(1);
        let mut backlog = Backlog::<1>::new(4);
        backlog.push(&ring, [0], Keep::Always); // fills the ring
        backlog.push(&ring, [1], Keep::Cheap);
        backlog.push(&ring, [2], Keep::Press { pair: 7 });
        backlog.push(&ring, [3], Keep::Release { pair: 7 });
        backlog.push(&ring, [4], Keep::Press { pair: 8 });
        // Full: the next entry drops the cheap one.
        backlog.push(&ring, [5], Keep::Release { pair: 8 });
        assert!(ring.take_dropped());
        // Then the oldest complete pair (2, 3).
        backlog.push(&ring, [6], Keep::Press { pair: 9 });
        // Then the oldest complete pair again (4, 5): only presses with releases are paired.
        backlog.push(&ring, [7], Keep::Release { pair: 1 });
        backlog.push(&ring, [8], Keep::Release { pair: 2 });
        // Now [6 press 9, 7 release, 8 release]: the lone press 9 goes, and its later release is dropped too.
        backlog.push(&ring, [10], Keep::Release { pair: 3 });
        backlog.push(&ring, [11], Keep::Release { pair: 9 });
        assert_eq!(backlog.len(), 3);
        let mut out = Vec::new();
        while let Some(w) = ring.peek() {
            out.push(w[0]);
            ring.pop();
            backlog.flush(&ring);
        }
        assert_eq!(out, [0, 7, 8, 10]);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn a_second_writer_fails_a_debug_build() {
        let ring = std::sync::Arc::new(Ring::<1>::new(4));
        ring.push([1]);
        let other = std::sync::Arc::clone(&ring);
        let result = std::thread::spawn(move || other.push([2])).join();
        assert!(result.is_err(), "the second thread's write panicked");
    }
}
