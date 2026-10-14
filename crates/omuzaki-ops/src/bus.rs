//! Event fan-out: every subscriber gets its own channel.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use omuzaki_ops_api::Event;

use crate::sync::lock;

/// The set of live subscribers.
#[derive(Default)]
pub(crate) struct Bus {
    subscribers: Mutex<Vec<Sender<Event>>>,
}

impl Bus {
    /// A new stream of every event delivered from now on.
    pub(crate) fn subscribe(&self) -> Receiver<Event> {
        let (sender, receiver) = channel();
        lock(&self.subscribers).push(sender);
        receiver
    }

    /// Sends the events `drain` returns to every subscriber, dropping those whose receiver is gone.
    ///
    /// The subscriber lock is taken *before* `drain` runs, and delivery batches are therefore serialised: whichever
    /// thread delivers next sends everything queued so far, in queue order. That keeps events in log order even when
    /// several threads run ops at once, without ever holding the executor's document lock while sending (`drain`
    /// takes it only for the instant it needs to empty the queue). Channels are unbounded, so a slow subscriber can
    /// never block an op.
    pub(crate) fn deliver(&self, drain: impl FnOnce() -> Vec<Event>) {
        let mut subscribers = lock(&self.subscribers);
        for event in drain() {
            subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_doc_api::DocVersion;
    use omuzaki_ops_api::{Actor, TxId};

    use super::*;

    fn rolled_back(n: u64) -> Event {
        Event::TxRolledBack { tx: TxId(n), actor: Actor::System, version: DocVersion(n) }
    }

    #[test]
    fn each_subscriber_gets_every_event_in_order() {
        let bus = Bus::default();
        let (a, b) = (bus.subscribe(), bus.subscribe());
        bus.deliver(|| vec![rolled_back(1), rolled_back(2)]);
        for rx in [a, b] {
            assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![rolled_back(1), rolled_back(2)]);
        }
    }

    #[test]
    fn dropped_receivers_are_unsubscribed_on_the_next_delivery() {
        let bus = Bus::default();
        let (kept, dropped) = (bus.subscribe(), bus.subscribe());
        drop(dropped);
        assert_eq!(lock(&bus.subscribers).len(), 2, "removal is lazy: nothing to notice until a send fails");
        bus.deliver(|| vec![rolled_back(1)]);
        assert_eq!(lock(&bus.subscribers).len(), 1);
        assert_eq!(kept.try_iter().count(), 1);
    }

    #[test]
    fn late_subscribers_see_only_later_events() {
        let bus = Bus::default();
        bus.deliver(|| vec![rolled_back(1)]);
        let late = bus.subscribe();
        bus.deliver(|| vec![rolled_back(2)]);
        assert_eq!(late.try_iter().collect::<Vec<_>>(), vec![rolled_back(2)]);
    }
}
