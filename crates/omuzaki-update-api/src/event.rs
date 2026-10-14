//! Update events: one bounded queue per subscriber, oldest dropped and counted (as `AudioEvents`).
//!
//! `omuzaki-core` has no shared `Subscription<T>` yet; when it gains one, `UpdateEvents` can wrap it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::status::UpdateStatus;

/// How many events one subscriber's queue holds.
pub const EVENT_QUEUE_CAPACITY: usize = 64;

/// Something the updater reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "detail", rename_all = "snake_case")]
pub enum UpdateEvent {
    /// The status changed.
    Status(UpdateStatus),
    /// Download progress. The sender sends at most four a second.
    Progress {
        /// Bytes so far.
        done: u64,
        /// Bytes in all.
        total: u64,
    },
}

#[derive(Debug, Default)]
struct EventQueue {
    events: VecDeque<UpdateEvent>,
    dropped: u64,
}

/// The updater's end of one subscriber's queue. Cloneable.
#[derive(Clone, Debug)]
pub struct EventSender {
    queue: Arc<Mutex<EventQueue>>,
}

/// One subscriber's queue (`Updater::subscribe`). When it is full, the oldest event is dropped and counted.
#[derive(Debug)]
pub struct UpdateEvents {
    queue: Arc<Mutex<EventQueue>>,
}

/// A new queue: the updater keeps the sender, the subscriber gets the receiver.
#[must_use]
pub fn event_queue() -> (EventSender, UpdateEvents) {
    let queue = Arc::new(Mutex::new(EventQueue::default()));
    (EventSender { queue: Arc::clone(&queue) }, UpdateEvents { queue })
}

fn locked(queue: &Mutex<EventQueue>) -> MutexGuard<'_, EventQueue> {
    // A panic elsewhere cannot corrupt a deque of plain values, so a poisoned lock is still safe to use.
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

impl EventSender {
    /// Queues `event`, dropping the oldest one first if the queue is full.
    pub fn send(&self, event: UpdateEvent) {
        let mut queue = locked(&self.queue);
        if queue.events.len() >= EVENT_QUEUE_CAPACITY {
            queue.events.pop_front();
            queue.dropped += 1;
        }
        queue.events.push_back(event);
    }

    /// Whether the subscriber still exists, so the updater can prune senders.
    #[must_use]
    pub fn is_subscribed(&self) -> bool {
        Arc::strong_count(&self.queue) > 1
    }
}

impl UpdateEvents {
    /// The oldest waiting event, if any.
    #[must_use]
    pub fn next(&self) -> Option<UpdateEvent> {
        locked(&self.queue).events.pop_front()
    }

    /// How many events were dropped because the queue was full, over the queue's life.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        locked(&self.queue).dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seventy_events_leave_the_newest_sixty_four() {
        let (tx, rx) = event_queue();
        for done in 0..70 {
            tx.send(UpdateEvent::Progress { done, total: 70 });
        }
        assert_eq!(rx.dropped(), 6);
        let mut seen = Vec::new();
        while let Some(UpdateEvent::Progress { done, .. }) = rx.next() {
            seen.push(done);
        }
        assert_eq!(seen, (6..70).collect::<Vec<_>>());
    }

    #[test]
    fn a_dropped_receiver_is_noticed() {
        let (tx, rx) = event_queue();
        assert!(tx.is_subscribed());
        drop(rx);
        assert!(!tx.is_subscribed());
    }
}
