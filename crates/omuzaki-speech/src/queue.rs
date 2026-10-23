//! A bounded queue with a *drop-oldest* back-pressure policy, shared by the utterance hand-off (VAD thread to ASR
//! thread) and the event stream (workers to the application).
//!
//! Neither side ever blocks on a full queue: a producer that finds it full evicts the oldest item and gets it back, so
//! it can report what was lost. For live speech the newest utterance is the one worth transcribing; a backlog of old
//! ones only delays it. The queue also has one *ephemeral* slot for superseded-by-newer work (partial transcripts):
//! setting it replaces the previous value, and any queued item supersedes it.
//!
//! These queues sit between worker threads and the application, never on the audio thread, so a mutex is fine.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

struct State<T> {
    items: VecDeque<T>,
    ephemeral: Option<T>,
    closed: bool,
}

/// A bounded multi-producer multi-consumer queue that evicts its oldest item when full.
pub struct DropOldestQueue<T> {
    capacity: usize,
    state: Mutex<State<T>>,
    ready: Condvar,
}

impl<T> DropOldestQueue<T> {
    /// A queue holding at most `capacity` items (at least one), with the storage allocated up front.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            state: Mutex::new(State { items: VecDeque::with_capacity(capacity), ephemeral: None, closed: false }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends `item`. When the queue is full the oldest item is evicted and returned so the caller can report the
    /// loss. A pending ephemeral item is discarded: the new item supersedes it. A closed queue refuses the item and
    /// hands it back as the "evicted" value.
    pub fn push(&self, item: T) -> Option<T> {
        let mut state = self.lock();
        if state.closed {
            return Some(item);
        }
        state.ephemeral = None;
        let evicted = if state.items.len() == self.capacity { state.items.pop_front() } else { None };
        state.items.push_back(item);
        drop(state);
        self.ready.notify_one();
        evicted
    }

    /// Sets the ephemeral slot, replacing (and returning) whatever was waiting there. It is served only when no
    /// regular item is queued. A closed queue refuses it.
    pub fn set_ephemeral(&self, item: T) -> Option<T> {
        let mut state = self.lock();
        if state.closed {
            return Some(item);
        }
        let replaced = state.ephemeral.replace(item);
        drop(state);
        self.ready.notify_one();
        replaced
    }

    /// The next item without waiting: regular items first (oldest first), then the ephemeral one.
    pub fn try_pop(&self) -> Option<T> {
        Self::take(&mut self.lock())
    }

    /// Waits up to `timeout` for the next item. `None` on timeout, or when the queue is closed and drained.
    pub fn pop_timeout(&self, timeout: Duration) -> Option<T> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if let Some(item) = Self::take(&mut state) {
                return Some(item);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if state.closed || left.is_zero() {
                return None;
            }
            state = self.ready.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Waits for the next item. `None` once the queue is closed and drained.
    pub fn pop(&self) -> Option<T> {
        let mut state = self.lock();
        loop {
            if let Some(item) = Self::take(&mut state) {
                return Some(item);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn take(state: &mut State<T>) -> Option<T> {
        state.items.pop_front().or_else(|| state.ephemeral.take())
    }

    /// Refuses new items and wakes every waiter. Items already queued can still be popped.
    pub fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
    }

    /// Drops everything queued (used when the host is aborted rather than drained).
    pub fn clear(&self) {
        let mut state = self.lock();
        state.items.clear();
        state.ephemeral = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_evicts_the_oldest_and_returns_it() {
        let q = DropOldestQueue::new(2);
        assert_eq!(q.push(1), None);
        assert_eq!(q.push(2), None);
        assert_eq!(q.push(3), Some(1));
        assert_eq!(q.try_pop(), Some(2));
        assert_eq!(q.try_pop(), Some(3));
        assert_eq!(q.try_pop(), None);
    }

    #[test]
    fn the_ephemeral_slot_keeps_only_the_latest_and_yields_to_regular_items() {
        let q = DropOldestQueue::new(2);
        assert_eq!(q.set_ephemeral("a"), None);
        assert_eq!(q.set_ephemeral("b"), Some("a"));
        assert_eq!(q.try_pop(), Some("b"));
        q.set_ephemeral("c");
        q.push("final"); // supersedes the pending ephemeral item
        assert_eq!(q.try_pop(), Some("final"));
        assert_eq!(q.try_pop(), None);
    }

    #[test]
    fn pop_waits_for_a_push_and_returns_none_after_close() {
        let q = std::sync::Arc::new(DropOldestQueue::new(1));
        let worker = {
            let q = std::sync::Arc::clone(&q);
            std::thread::spawn(move || (q.pop(), q.pop()))
        };
        std::thread::sleep(Duration::from_millis(20));
        q.push(7);
        std::thread::sleep(Duration::from_millis(20));
        q.close();
        assert_eq!(worker.join().unwrap(), (Some(7), None));
        assert_eq!(q.push(8), Some(8), "a closed queue refuses new items");
    }

    #[test]
    fn pop_timeout_gives_up() {
        let q: DropOldestQueue<u8> = DropOldestQueue::new(1);
        let started = Instant::now();
        assert_eq!(q.pop_timeout(Duration::from_millis(30)), None);
        assert!(started.elapsed() >= Duration::from_millis(25));
    }
}
