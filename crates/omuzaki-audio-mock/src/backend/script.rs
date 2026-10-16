//! The script that drives a [`FakeBackend`](super::FakeBackend): what happens to the devices, and at which sample.

use omuzaki_audio_api::device::DeviceId;
use omuzaki_core::time::Seconds;

/// One thing that happens to the fake host.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The device disappears: it leaves the enumeration, and every stream open on it gets
    /// [`BackendError::DeviceNotAvailable`](omuzaki_audio_api::backend::BackendError::DeviceNotAvailable) on its error
    /// callback and stops.
    Vanish(DeviceId),
    /// The device comes back. Streams that died with it stay dead: the engine has to open them again, as with a real
    /// host.
    Return(DeviceId),
    /// The enumeration lists these devices first, in this order, then the rest in their previous order. Ids that are
    /// not present are ignored.
    Reorder(Vec<DeviceId>),
    /// The device comes back with the same label and a new id (a driver restart on Windows). Streams open on the old id
    /// die as they do on [`Event::Vanish`]; the old id is gone from the host.
    Reid {
        /// The id the device had.
        from: DeviceId,
        /// The id it has from now on.
        to: DeviceId,
    },
    /// The device's clock now runs at `1 + ppm / 1e6` of the nominal rate: its callbacks come that much faster.
    Drift {
        /// The device.
        device: DeviceId,
        /// Parts per million; negative is slow.
        ppm: f64,
    },
    /// The device's capture-to-callback (input) or callback-to-playback (output) latency changes.
    Latency {
        /// The device.
        device: DeviceId,
        /// The new latency.
        latency: Seconds,
    },
    /// The OS default for the device's direction becomes this device.
    SetDefault(DeviceId),
}

/// Events by sample index of the engine timeline (48 kHz). An event at index `k` takes effect before sample `k`: every
/// callback due at or after `k / 48000` s sees it, and none before.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    events: Vec<(u64, Event)>,
}

impl Script {
    /// A script with nothing in it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `event` at sample `index`. Events at the same index happen in the order they were added.
    #[must_use]
    pub fn at(mut self, index: u64, event: Event) -> Self {
        self.push(index, event);
        self
    }

    pub(crate) fn push(&mut self, index: u64, event: Event) {
        let at = self.events.partition_point(|(i, _)| *i <= index);
        self.events.insert(at, (index, event));
    }

    /// The events not yet applied.
    #[must_use]
    pub fn events(&self) -> &[(u64, Event)] {
        &self.events
    }

    /// The index of the next event, if any.
    pub(crate) fn next_index(&self) -> Option<u64> {
        self.events.first().map(|(i, _)| *i)
    }

    /// Removes and returns the first event if it is due at `now`.
    pub(crate) fn pop_due(&mut self, now: u64) -> Option<Event> {
        (self.events.first()?.0 <= now).then(|| self.events.remove(0).1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> DeviceId {
        DeviceId { host: "fake".into(), id: s.into() }
    }

    #[test]
    fn events_sort_by_index_and_keep_insertion_order_within_one() {
        let script = Script::new()
            .at(50, Event::Vanish(id("a")))
            .at(10, Event::Return(id("b")))
            .at(50, Event::Return(id("a")))
            .at(10, Event::SetDefault(id("c")));
        let order: Vec<(u64, &Event)> = script.events().iter().map(|(i, e)| (*i, e)).collect();
        assert_eq!(
            order,
            [
                (10, &Event::Return(id("b"))),
                (10, &Event::SetDefault(id("c"))),
                (50, &Event::Vanish(id("a"))),
                (50, &Event::Return(id("a")))
            ]
        );
        assert_eq!(script.next_index(), Some(10));
    }

    #[test]
    fn pop_due_releases_only_what_has_come() {
        let mut script = Script::new().at(5, Event::Vanish(id("a")));
        assert_eq!(script.pop_due(4), None);
        assert_eq!(script.pop_due(5), Some(Event::Vanish(id("a"))));
        assert_eq!(script.pop_due(99), None);
    }
}
