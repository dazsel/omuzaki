//! Change records and the event stream (D6).
//!
//! One [`SettingsEvent`] per change set: a `settings.set` is one, a profile switch is one carrying every key that
//! changed. Consumers (the renderer, the audio engine) copy the value they need into their own state when their key
//! changes and use that copy on the frame path; they never read settings per frame.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, PoisonError};

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::Value;
use omuzaki_ops_api::Actor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::meta::Apply;
use crate::profile::ProfileId;

/// One key's change. `before` and `after` are the effective values, so a write hidden by a profile override is no
/// change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Change {
    /// The key.
    pub key: String,
    /// The effective value before.
    pub before: Value,
    /// The effective value after.
    pub after: Value,
    /// The key's `Apply`, so the UI can say which changes wait for a restart.
    pub apply: Apply,
}

impl Change {
    /// A change of `key` from `before` to `after`.
    #[must_use]
    pub fn new(key: impl Into<String>, before: Value, after: Value, apply: Apply) -> Self {
        Self { key: key.into(), before, after, apply }
    }
}

/// Why a set of changes happened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cause", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangeCause {
    /// `settings.set`.
    Set,
    /// `settings.reset`.
    Reset,
    /// The active profile changed (or was deleted, which switches to none first).
    ProfileSwitched {
        /// The profile before.
        from: Option<ProfileId>,
        /// The profile after.
        to: Option<ProfileId>,
    },
    /// The files were re-read (`settings.reload`), or the active profile was edited.
    Reloaded,
    /// Per-key migrations ran at load.
    Migrated,
}

/// What subscribers receive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SettingsEvent {
    /// Why.
    pub cause: ChangeCause,
    /// The keys whose effective value changed; empty only when the active profile changed and no value did.
    pub changes: Vec<Change>,
}

impl SettingsEvent {
    /// An event for `cause` listing `changes`.
    #[must_use]
    pub fn new(cause: ChangeCause, changes: Vec<Change>) -> Self {
        Self { cause, changes }
    }

    /// The change of `key`, if this event has one: the consumer's "did my key change?" test.
    #[must_use]
    pub fn change_of(&self, key: &str) -> Option<&Change> {
        self.changes.iter().find(|c| c.key == key)
    }
}

/// What `Settings::switch_profile` reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SwitchReport {
    /// How many keys changed value.
    pub changed: u32,
    /// Changed keys whose `Apply` is `NextStart`: the UI says "applies after restart".
    pub pending_restart: Vec<String>,
}

impl SwitchReport {
    /// A report of `changed` keys, `pending_restart` of which wait for a restart.
    #[must_use]
    pub fn new(changed: u32, pending_restart: Vec<String>) -> Self {
        Self { changed, pending_restart }
    }
}

/// One entry of the in-memory change log (the last 200), which is how the UI shows "changed by an agent 2 min ago".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SettingChange {
    /// The change.
    pub change: Change,
    /// Who made it.
    pub actor: Actor,
    /// When, on the service's clock.
    pub at: Seconds,
}

impl SettingChange {
    /// `change`, made by `actor` at `at`.
    #[must_use]
    pub fn new(change: Change, actor: Actor, at: Seconds) -> Self {
        Self { change, actor, at }
    }
}

/// How many entries the change log keeps.
pub const CHANGE_LOG_CAPACITY: usize = 200;

/// One subscriber's queue of events, oldest first. Each subscriber has its own, so the UI and the renderer never
/// steal each other's events. The queue is unbounded: settings change at human speed.
#[derive(Debug)]
pub struct Subscription<T> {
    rx: Receiver<T>,
}

impl<T> Subscription<T> {
    /// The oldest waiting event, if any. Never blocks.
    #[must_use]
    pub fn next(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }

    /// Takes every waiting event, oldest first.
    #[must_use]
    pub fn drain(&self) -> Vec<T> {
        self.rx.try_iter().collect()
    }
}

/// The service's end: fans each event out to every live subscriber. Implementations of `Settings` embed one, so the
/// delivery rules (order, one event to each subscriber, dead subscribers dropped) are defined once.
#[derive(Debug)]
pub struct Publisher<T> {
    senders: Mutex<Vec<Sender<T>>>,
}

impl<T> Default for Publisher<T> {
    fn default() -> Self {
        Self { senders: Mutex::new(Vec::new()) }
    }
}

impl<T: Clone> Publisher<T> {
    /// A new subscriber. It sees events published after this call.
    #[must_use]
    pub fn subscribe(&self) -> Subscription<T> {
        let (tx, rx) = mpsc::channel();
        self.senders.lock().unwrap_or_else(PoisonError::into_inner).push(tx);
        Subscription { rx }
    }

    /// Delivers `event` to every live subscriber and forgets the dropped ones.
    pub fn publish(&self, event: &T) {
        let mut senders = self.senders.lock().unwrap_or_else(PoisonError::into_inner);
        senders.retain(|tx| tx.send(event.clone()).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_subscriber_gets_every_event_in_order_and_dead_ones_are_dropped() {
        let hub = Publisher::<u32>::default();
        let (a, b) = (hub.subscribe(), hub.subscribe());
        hub.publish(&1);
        drop(b);
        hub.publish(&2);
        assert_eq!(a.drain(), vec![1, 2]);
        assert_eq!(a.next(), None);
        assert_eq!(hub.senders.lock().unwrap().len(), 1);
    }

    #[test]
    fn events_round_trip_through_json() {
        let e = SettingsEvent::new(
            ChangeCause::ProfileSwitched { from: None, to: ProfileId::new("stream-k3f9").ok() },
            vec![Change::new("quality.tier", "live".into(), "high".into(), Apply::Live)],
        );
        let back: SettingsEvent = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        assert!(e.change_of("quality.tier").is_some() && e.change_of("ui.scale").is_none());
    }
}
