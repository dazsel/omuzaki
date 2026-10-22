//! The running show's inbox (design D8, D9): events from producers and replies from the dispatcher, in arrival order.
//!
//! Bounded so a stalled show can't eat memory: droppable kinds (chat) are shed past
//! [`INBOX_DROPPABLE_CAPACITY`] and counted; everything else is held up to [`INBOX_HARD_CAP`], past which `publish`
//! answers `Full` and the producer keeps and retries it. Replies are never refused: their calls are already in
//! flight and bounded by [`MAX_CALLS_IN_FLIGHT`](omuzaki_live_api::limits::MAX_CALLS_IN_FLIGHT). Platform
//! redeliveries are dropped: an event whose (platform, kind, origin id) was among the last [`DEDUP_EVENTS`] of that
//! platform within [`DEDUP_SECONDS`] never reaches a graph twice.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::{EventId, LiveEvent, PublishError, Source};
use omuzaki_live_api::limits::{DEDUP_EVENTS, DEDUP_SECONDS, INBOX_DROPPABLE_CAPACITY, INBOX_HARD_CAP};

use crate::core::Item;

/// One platform's recent events: (kind, origin id) in arrival order with their times, and the same keys as a set, so
/// a redelivery is found in one lookup instead of a scan of the window.
#[derive(Default)]
struct Seen {
    order: VecDeque<((String, String), f64)>,
    keys: HashSet<(String, String)>,
}

impl Seen {
    fn pop_front(&mut self) {
        if let Some((key, _)) = self.order.pop_front() {
            self.keys.remove(&key);
        }
    }
}

#[derive(Default)]
struct State {
    queue: VecDeque<(Item, bool)>,
    droppable: usize,
    closed: bool,
    seen: BTreeMap<String, Seen>,
    dropped_inbox: u64,
    dropped_full: u64,
    deduplicated: u64,
    /// Whether the engine thread sleeps in `pop`: a push wakes it only then, so a busy engine costs producers no
    /// wake-up call.
    waiting: bool,
}

/// How long the engine thread keeps looking for more before it sleeps: a burst (a raid's chat) then costs producers
/// no wake-up call per event, and a quiet show sleeps after this.
const SPIN: Duration = Duration::from_micros(50);

/// The inbox of one run.
#[derive(Default)]
pub(crate) struct Inbox {
    state: Mutex<State>,
    ready: Condvar,
    /// The queue's length, readable without the lock, for the engine's short spin before it sleeps.
    queued: AtomicUsize,
}

impl Inbox {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues an event that already passed `publish`'s checks, stamping it with its arrival: `at` on the engine clock
    /// and the next id from `ids`, taken under the inbox's lock so ids rise in exactly the order events are queued.
    /// Answers the id, or `None` for a redelivery dropped silently (or a closed inbox).
    ///
    /// # Errors
    /// [`PublishError::Full`] for a droppable kind past its capacity (dropped and counted) and for any kind past the
    /// hard cap (the producer retries).
    pub(crate) fn push_event(
        &self,
        mut event: LiveEvent,
        droppable: bool,
        at: Seconds,
        ids: &AtomicU64,
    ) -> Result<Option<EventId>, PublishError> {
        let mut state = self.lock();
        if state.closed {
            return Ok(None);
        }
        if let (Source::Platform { platform, .. }, Some(origin)) = (&event.source, &event.origin_id) {
            let recent = state.seen.entry(platform.to_string()).or_default();
            while recent.order.front().is_some_and(|(_, t)| at.0 - t > DEDUP_SECONDS) {
                recent.pop_front();
            }
            let key = (event.kind.as_str().to_owned(), origin.clone());
            if recent.keys.contains(&key) {
                state.deduplicated += 1;
                return Ok(None);
            }
            if recent.order.len() >= DEDUP_EVENTS {
                recent.pop_front();
            }
            recent.keys.insert(key.clone());
            recent.order.push_back((key, at.0));
        }
        if droppable && state.droppable >= INBOX_DROPPABLE_CAPACITY {
            state.dropped_inbox += 1;
            return Err(PublishError::Full);
        }
        if state.queue.len() >= INBOX_HARD_CAP {
            state.dropped_full += 1;
            return Err(PublishError::Full);
        }
        if droppable {
            state.droppable += 1;
        }
        let id = EventId(ids.fetch_add(1, Ordering::Relaxed) + 1);
        event.id = id;
        event.at = at;
        state.queue.push_back((Item::Event(event), droppable));
        self.queued.store(state.queue.len(), Ordering::Release);
        let wake = state.waiting;
        drop(state);
        if wake {
            self.ready.notify_one();
        }
        Ok(Some(id))
    }

    /// Queues an op reply.
    pub(crate) fn push_reply(&self, item: Item) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        state.queue.push_back((item, false));
        self.queued.store(state.queue.len(), Ordering::Release);
        let wake = state.waiting;
        drop(state);
        if wake {
            self.ready.notify_one();
        }
    }

    /// The next item, waiting up to `timeout`; `None` on timeout or once closed.
    #[cfg(test)]
    pub(crate) fn pop(&self, timeout: Duration) -> Option<Item> {
        let mut batch = VecDeque::new();
        self.pop_batch(timeout, 1, &mut batch);
        batch.pop_front()
    }

    /// Moves up to `max` items, in order, into `out`, waiting up to `timeout` for the first; nothing on timeout or
    /// once closed. One lock for many items, so a busy engine and its producers rarely meet on it.
    pub(crate) fn pop_batch(&self, timeout: Duration, max: usize, out: &mut VecDeque<Item>) {
        let spin_until = std::time::Instant::now() + SPIN.min(timeout);
        while self.queued.load(Ordering::Acquire) == 0 && std::time::Instant::now() < spin_until {
            std::hint::spin_loop();
        }
        let mut state = self.lock();
        if state.queue.is_empty() && !state.closed {
            state.waiting = true;
            state = self
                .ready
                .wait_timeout_while(state, timeout, |s| s.queue.is_empty() && !s.closed)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            state.waiting = false;
        }
        if state.closed {
            return;
        }
        for _ in 0..max {
            let Some((item, droppable)) = state.queue.pop_front() else { break };
            if droppable {
                state.droppable = state.droppable.saturating_sub(1);
            }
            out.push_back(item);
        }
        self.queued.store(state.queue.len(), Ordering::Release);
    }

    /// Whether nothing is waiting.
    pub(crate) fn is_empty(&self) -> bool {
        self.lock().queue.is_empty()
    }

    /// Stops the inbox: queued items are dropped, later pushes ignored, and a waiting `pop` returns at once.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.queue.clear();
        self.queued.store(0, Ordering::Release);
        drop(state);
        self.ready.notify_all();
    }

    /// (droppable events shed, events refused past the hard cap, redeliveries dropped).
    pub(crate) fn dropped(&self) -> (u64, u64, u64) {
        let state = self.lock();
        (state.dropped_inbox, state.dropped_full, state.deduplicated)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use omuzaki_live_api::event::PublishError;
    use omuzaki_live_mock::fixtures;

    use super::*;

    fn event_of(item: Item) -> LiveEvent {
        match item {
            Item::Event(event) => event,
            Item::Reply { .. } | Item::Control(_) | Item::Reload(_) => panic!("a reply where an event was queued"),
        }
    }

    #[test]
    fn eight_producers_lose_nothing_and_ids_rise_in_queue_order() {
        const PRODUCERS: usize = 8;
        const EACH: usize = 10_000;
        let inbox = Arc::new(Inbox::default());
        let ids = Arc::new(AtomicU64::new(0));
        let consumer = {
            let inbox = Arc::clone(&inbox);
            std::thread::spawn(move || {
                let mut seen = Vec::with_capacity(PRODUCERS * EACH);
                while seen.len() < PRODUCERS * EACH {
                    if let Some(item) = inbox.pop(Duration::from_secs(10)) {
                        seen.push(event_of(item));
                    } else {
                        break;
                    }
                }
                seen
            })
        };
        let producers: Vec<_> = (0..PRODUCERS)
            .map(|p| {
                let (inbox, ids) = (Arc::clone(&inbox), Arc::clone(&ids));
                std::thread::spawn(move || {
                    for i in 0..EACH {
                        // Distinct origin ids, so dedup keeps every one.
                        let event = fixtures::chat(&format!("p{p}"), &format!("{i}"));
                        // A producer keeps an event the inbox refused and retries it (design D9).
                        while let Err(PublishError::Full) = inbox.push_event(event.clone(), true, Seconds(0.0), &ids) {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();
        for producer in producers {
            producer.join().unwrap();
        }
        let seen = consumer.join().unwrap();
        assert_eq!(seen.len(), PRODUCERS * EACH, "none lost");
        assert!(seen.windows(2).all(|w| w[0].id < w[1].id), "ids rise strictly in the order events were queued");
        // And each producer's events arrive in the order it sent them.
        for p in 0..PRODUCERS {
            let name = format!("p{p}");
            let texts: Vec<usize> = seen
                .iter()
                .filter(|e| e.payload["user"]["name"] == name.as_str())
                .map(|e| e.payload["text"].as_str().unwrap().parse().unwrap())
                .collect();
            assert_eq!(texts, (0..EACH).collect::<Vec<_>>());
        }
    }

    #[test]
    fn a_full_droppable_queue_sheds_chat_and_still_takes_a_redeem() {
        let inbox = Inbox::default();
        let ids = AtomicU64::new(0);
        for i in 0..INBOX_DROPPABLE_CAPACITY {
            let chat = fixtures::chat("kiri", &format!("{i}"));
            assert!(inbox.push_event(chat, true, Seconds(0.0), &ids).unwrap().is_some());
        }
        let one_more = fixtures::chat("kiri", "one too many");
        assert_eq!(inbox.push_event(one_more, true, Seconds(0.0), &ids), Err(PublishError::Full));
        let redeem = fixtures::redeem("kiri", "BRB");
        assert!(inbox.push_event(redeem, false, Seconds(0.0), &ids).unwrap().is_some(), "the redeem is taken");
        assert_eq!(inbox.dropped(), (1, 0, 0), "the shed chat is counted");
        let mut last = None;
        while let Some(item) = inbox.pop(Duration::ZERO) {
            last = Some(event_of(item));
        }
        assert_eq!(last.unwrap().kind.as_str(), "redeem", "the redeem is delivered after the chat before it");
    }

    #[test]
    fn a_redelivery_within_ten_minutes_is_dropped_and_one_after_is_not() {
        let inbox = Inbox::default();
        let ids = AtomicU64::new(0);
        let redeem = fixtures::redeem("kiri", "BRB");
        assert!(inbox.push_event(redeem.clone(), false, Seconds(0.0), &ids).unwrap().is_some());
        assert_eq!(inbox.push_event(redeem.clone(), false, Seconds(599.0), &ids), Ok(None), "dropped silently");
        assert_eq!(inbox.dropped().2, 1, "and counted");
        let later = inbox.push_event(redeem, false, Seconds(601.0), &ids).unwrap();
        assert!(later.is_some(), "ten minutes after the first, it is a new event");
        assert_eq!(later, Some(EventId(2)), "a dropped redelivery takes no id");
    }

    #[test]
    fn dedup_remembers_the_last_1024_events_of_a_platform() {
        let inbox = Inbox::default();
        let ids = AtomicU64::new(0);
        let first = fixtures::chat("kiri", "first");
        assert!(inbox.push_event(first.clone(), false, Seconds(0.0), &ids).unwrap().is_some());
        for i in 0..DEDUP_EVENTS {
            let chat = fixtures::chat("kiri", &format!("{i}"));
            assert!(inbox.push_event(chat, false, Seconds(1.0), &ids).unwrap().is_some());
        }
        assert!(
            inbox.push_event(first, false, Seconds(2.0), &ids).unwrap().is_some(),
            "1,024 later events pushed the first out of the window"
        );
    }
}
