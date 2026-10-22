//! The running show's inbox (design D8, D9): events from producers and replies from the dispatcher, in arrival order.
//!
//! Bounded so a stalled show can't eat memory: droppable kinds (chat) are shed past
//! [`INBOX_DROPPABLE_CAPACITY`] and counted; everything else is held up to [`INBOX_HARD_CAP`], past which `publish`
//! answers `Full` and the producer keeps and retries it. Replies are never refused: their calls are already in
//! flight and bounded by [`MAX_CALLS_IN_FLIGHT`](omuzaki_live_api::limits::MAX_CALLS_IN_FLIGHT). Platform
//! redeliveries are dropped: an event whose (platform, kind, origin id) was among the last [`DEDUP_EVENTS`] of that
//! platform within [`DEDUP_SECONDS`] never reaches a graph twice.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::{LiveEvent, PublishError, Source};
use omuzaki_live_api::limits::{DEDUP_EVENTS, DEDUP_SECONDS, INBOX_DROPPABLE_CAPACITY, INBOX_HARD_CAP};

use crate::core::Item;

/// One recent platform event: its kind, origin id and arrival time.
type Seen = (String, String, f64);

#[derive(Default)]
struct State {
    queue: VecDeque<(Item, bool)>,
    droppable: usize,
    closed: bool,
    seen: BTreeMap<String, VecDeque<Seen>>,
    dropped_inbox: u64,
    dropped_full: u64,
}

/// The inbox of one run.
#[derive(Default)]
pub(crate) struct Inbox {
    state: Mutex<State>,
    ready: Condvar,
}

impl Inbox {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues an event that already passed `publish`'s checks; `at` is its arrival on the engine clock.
    ///
    /// # Errors
    /// [`PublishError::Full`] for a droppable kind past its capacity (dropped and counted) and for any kind past the
    /// hard cap (the producer retries).
    pub(crate) fn push_event(&self, event: LiveEvent, droppable: bool, at: Seconds) -> Result<(), PublishError> {
        let mut state = self.lock();
        if state.closed {
            return Ok(());
        }
        if let (Source::Platform { platform, .. }, Some(origin)) = (&event.source, &event.origin_id) {
            let recent = state.seen.entry(platform.to_string()).or_default();
            while recent.front().is_some_and(|(_, _, t)| at.0 - t > DEDUP_SECONDS) || recent.len() > DEDUP_EVENTS {
                recent.pop_front();
            }
            let kind = event.kind.as_str();
            if recent.iter().any(|(k, o, _)| k == kind && o == origin) {
                return Ok(());
            }
            recent.push_back((kind.to_owned(), origin.clone(), at.0));
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
        state.queue.push_back((Item::Event(event), droppable));
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    /// Queues an op reply.
    pub(crate) fn push_reply(&self, item: Item) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        state.queue.push_back((item, false));
        drop(state);
        self.ready.notify_one();
    }

    /// The next item, waiting up to `timeout`; `None` on timeout or once closed.
    pub(crate) fn pop(&self, timeout: Duration) -> Option<Item> {
        let state = self.lock();
        let (mut state, _) = self
            .ready
            .wait_timeout_while(state, timeout, |s| s.queue.is_empty() && !s.closed)
            .unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return None;
        }
        let (item, droppable) = state.queue.pop_front()?;
        if droppable {
            state.droppable = state.droppable.saturating_sub(1);
        }
        Some(item)
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
        drop(state);
        self.ready.notify_all();
    }

    /// (droppable events shed, events refused past the hard cap).
    pub(crate) fn dropped(&self) -> (u64, u64) {
        let state = self.lock();
        (state.dropped_inbox, state.dropped_full)
    }
}
