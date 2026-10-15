//! [`EventHub`]: the host's own single-consumer event queue (D4).
//!
//! An mpsc channel cannot take back what it holds, and Stop must: after `stop()` returns the panel may not show one
//! more word. So the queue is a plain list under one lock, and [`EventHub::stop`] sets the stopped flag and withdraws
//! the unread output in that same critical section. The real host and the mock share this type, so the guarantee is
//! tested once.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::event::AssistantEvent;

#[derive(Default)]
struct Shared {
    queue: VecDeque<AssistantEvent>,
    /// The generation of the receiver that may read; 0 while nobody has subscribed.
    consumer: u64,
    stopped: bool,
    /// Bytes of this answer's `TextDelta` text the consumer has read.
    text_read: usize,
}

/// What [`EventHub::stop`] did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StopReport {
    /// How many unread events were withdrawn.
    pub withdrawn: usize,
    /// Bytes of this answer's `TextDelta` text the consumer had read. The host cuts the answer's stored text to this,
    /// so the panel, `transcript()` and the model's next history agree.
    pub text_read: usize,
}

/// The queue between the host thread (which sends) and the panel (which drains with
/// [`EventReceiver::try_recv`]). Clones share one queue.
#[derive(Clone, Default)]
pub struct EventHub {
    shared: Arc<Mutex<Shared>>,
}

impl EventHub {
    /// An empty hub with no receiver.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        // A poisoned lock only means a thread panicked while queueing; the queue itself is still a plain list.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Becomes the one consumer. A later call detaches the earlier receiver, which then yields `None`. Events not yet
    /// read stay queued for the new receiver.
    pub fn subscribe(&self) -> EventReceiver {
        let mut shared = self.lock();
        shared.consumer += 1;
        EventReceiver { shared: Arc::clone(&self.shared), generation: shared.consumer }
    }

    /// Queues `event`. Returns `false`, queueing nothing, once [`stop`](Self::stop) has been called, except for
    /// `Paused` and `Done`, which are always accepted. With no receiver the event is dropped and `true` is returned:
    /// a panel opened late reads `transcript()` instead.
    pub fn send(&self, event: AssistantEvent) -> bool {
        let mut shared = self.lock();
        if shared.stopped && !matches!(event, AssistantEvent::Paused | AssistantEvent::Done) {
            return false;
        }
        if shared.consumer != 0 {
            shared.queue.push_back(event);
        }
        true
    }

    /// Stop's synchronous half, under the queue's lock: nothing but `Paused` and `Done` is accepted from now on, and
    /// every unread `TextDelta`, `ToolCall`, `Question` and `Plan` is withdrawn. Unread `ToolResult`, `Waiting`,
    /// `Budget`, `Limit`, `Error`, `Paused` and `Resumed` stay, because they are state, not output. Calling it again
    /// withdraws nothing and reports the same `text_read`.
    pub fn stop(&self) -> StopReport {
        let mut shared = self.lock();
        shared.stopped = true;
        let before = shared.queue.len();
        shared.queue.retain(|e| !e.is_withdrawn_by_stop());
        StopReport { withdrawn: before - shared.queue.len(), text_read: shared.text_read }
    }

    /// A new answer starts: output is accepted again and the count of text read starts over.
    pub fn reset(&self) {
        let mut shared = self.lock();
        shared.stopped = false;
        shared.text_read = 0;
    }
}

impl std::fmt::Debug for EventHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shared = self.lock();
        f.debug_struct("EventHub").field("queued", &shared.queue.len()).field("stopped", &shared.stopped).finish()
    }
}

/// The panel's end of an [`EventHub`].
pub struct EventReceiver {
    shared: Arc<Mutex<Shared>>,
    generation: u64,
}

impl EventReceiver {
    /// The next event, or `None` if there is none or this receiver has been detached by a later `subscribe`. Never
    /// blocks beyond the queue's lock, which is held for a few instructions.
    pub fn try_recv(&self) -> Option<AssistantEvent> {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if shared.consumer != self.generation {
            return None;
        }
        let event = shared.queue.pop_front()?;
        if let AssistantEvent::TextDelta(text) = &event {
            shared.text_read += text.len();
        }
        Some(event)
    }
}

impl std::fmt::Debug for EventReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventReceiver").field("generation", &self.generation).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::ApprovalId;

    use super::*;
    use crate::event::{PlanItem, PlanStatus, QuestionId};

    fn delta(text: &str) -> AssistantEvent {
        AssistantEvent::TextDelta(text.into())
    }

    #[test]
    fn stop_withdraws_the_unread_text_and_reports_what_was_read() {
        let hub = EventHub::new();
        let rx = hub.subscribe();
        for t in ["aa", "bbb", "c", "dd", "e"] {
            assert!(hub.send(delta(t)));
        }
        assert_eq!(rx.try_recv(), Some(delta("aa")));
        assert_eq!(rx.try_recv(), Some(delta("bbb")));
        let report = hub.stop();
        assert_eq!(report, StopReport { withdrawn: 3, text_read: 5 });
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn after_stop_only_paused_and_done_are_accepted() {
        let hub = EventHub::new();
        let rx = hub.subscribe();
        hub.stop();
        assert!(!hub.send(delta("late")));
        assert!(!hub.send(AssistantEvent::Resumed));
        assert!(hub.send(AssistantEvent::Paused));
        assert!(hub.send(AssistantEvent::Done));
        assert_eq!(rx.try_recv(), Some(AssistantEvent::Paused));
        assert_eq!(rx.try_recv(), Some(AssistantEvent::Done));
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn reset_accepts_output_again_and_restarts_the_count() {
        let hub = EventHub::new();
        let rx = hub.subscribe();
        hub.send(delta("abc"));
        rx.try_recv();
        assert_eq!(hub.stop().text_read, 3);
        hub.reset();
        assert!(hub.send(delta("x")));
        assert_eq!(hub.stop(), StopReport { withdrawn: 1, text_read: 0 });
    }

    #[test]
    fn plan_question_and_tool_call_are_withdrawn_but_waiting_and_results_stay() {
        let hub = EventHub::new();
        let rx = hub.subscribe();
        let kept = [
            AssistantEvent::tool_result("c", true),
            AssistantEvent::waiting(ApprovalId(1)),
            AssistantEvent::Limit(crate::event::Cap::Turns),
        ];
        hub.send(AssistantEvent::tool_call("c", "t", "s"));
        hub.send(AssistantEvent::Plan(vec![PlanItem::new("p", PlanStatus::Todo)]));
        hub.send(AssistantEvent::question(QuestionId(1), "q", Vec::new()));
        for e in &kept {
            hub.send(e.clone());
        }
        assert_eq!(hub.stop().withdrawn, 3);
        let left: Vec<_> = std::iter::from_fn(|| rx.try_recv()).collect();
        assert_eq!(left, kept);
    }

    #[test]
    fn a_second_subscribe_detaches_the_first_receiver() {
        let hub = EventHub::new();
        let first = hub.subscribe();
        hub.send(delta("a"));
        let second = hub.subscribe();
        assert_eq!(first.try_recv(), None);
        assert_eq!(second.try_recv(), Some(delta("a")));
        hub.send(delta("b"));
        assert_eq!(first.try_recv(), None);
        assert_eq!(second.try_recv(), Some(delta("b")));
    }

    #[test]
    fn with_no_receiver_events_are_dropped() {
        let hub = EventHub::new();
        assert!(hub.send(delta("a")));
        let rx = hub.subscribe();
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn stop_twice_changes_nothing() {
        let hub = EventHub::new();
        let rx = hub.subscribe();
        hub.send(delta("ab"));
        rx.try_recv();
        hub.send(delta("cd"));
        assert_eq!(hub.stop(), StopReport { withdrawn: 1, text_read: 2 });
        assert_eq!(hub.stop(), StopReport { withdrawn: 0, text_read: 2 });
    }
}
