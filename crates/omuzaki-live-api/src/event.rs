//! The event envelope (design D9): [`LiveEvent`], where it came from ([`Source`]), the self-registered event kinds
//! ([`register_event_kind!`]) and the [`EventSink`] producers publish into.
//!
//! BR-10's adapters, the input source, the voice listener and the OBS client publish from their own threads; nothing
//! polls. Kinds register with a payload schema, a summary and whether they may be dropped under load; the normalised
//! platform kinds are in [`payloads`], so a chat command works on every platform at once. Events live in memory only.

pub mod payloads;
mod registration;

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::Actor;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{EventKind, GraphId, NodeId, Platform};
use crate::limits::MAX_PAYLOAD_BYTES;

pub use registration::{EventKindRegistration, check_payload, event_kinds, find_event_kind};

/// One event: what happened, where it came from, when, and its payload. Built with [`LiveEvent::new`] or
/// [`LiveEvent::of`]; the inbox sets [`id`](Self::id), the engine [`hops`](Self::hops) and [`acting`](Self::acting).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct LiveEvent {
    /// The arrival number, set by the inbox; 0 until then.
    pub id: EventId,
    /// What kind of event (`chat.message`, `redeem`, `custom.bonk`).
    pub kind: EventKind,
    /// Who produced it.
    pub source: Source,
    /// The platform's own id for it, for de-duplication, replies and moderation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_id: Option<String>,
    /// When it happened on the runner clock, stamped by the producer.
    pub at: Seconds,
    /// The platform's own timestamp, when it sends one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc: Option<UtcMillis>,
    /// The payload: an object, checked against the kind's schema when published.
    pub payload: Value,
    /// How many `event.emit` hops led here (design D8); 0 for an event from outside.
    #[serde(default)]
    pub hops: u8,
    /// The caller a manual event runs as, copied by `event.emit` to the events it derives (design D7). `None` for
    /// every event from outside: the show's own session acts. [`EventSink::publish`] refuses an event that sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acting: Option<Actor>,
}

impl LiveEvent {
    /// An event of `kind` from `source` at `at`, with no platform id, timestamp, hops or acting caller.
    pub fn new(kind: EventKind, source: Source, at: Seconds, payload: Value) -> Self {
        Self { id: EventId(0), kind, source, origin_id: None, at, utc: None, payload, hops: 0, acting: None }
    }

    /// An event of `P`'s kind carrying `payload`.
    ///
    /// # Errors
    /// [`EventError::TooLarge`] for a payload over [`MAX_PAYLOAD_BYTES`] of JSON, [`EventError::NotAnObject`] if it
    /// doesn't serialise to an object.
    pub fn of<P: EventPayload>(payload: &P, source: Source, at: Seconds) -> Result<Self, EventError> {
        let kind = EventKind::new(P::KIND)
            .map_err(|e| EventError::Decode { kind: P::KIND.to_owned(), reason: e.to_string() })?;
        Self::of_kind(kind, payload, source, at)
    }

    /// An event of `kind`, one of the kinds `P` carries ([`EventPayload::is_kind`]): `show.stopping` with a
    /// [`ShowLifecycle`](payloads::ShowLifecycle), say.
    ///
    /// # Errors
    /// [`EventError::WrongKind`] if `P` doesn't carry `kind`; otherwise as [`of`](Self::of).
    pub fn of_kind<P: EventPayload>(
        kind: EventKind,
        payload: &P,
        source: Source,
        at: Seconds,
    ) -> Result<Self, EventError> {
        if !P::is_kind(kind.as_str()) {
            return Err(EventError::WrongKind { kind: kind.to_string(), payload: P::KIND });
        }
        let value = serde_json::to_value(payload).map_err(|e| EventError::from_serde(kind.as_str(), &e))?;
        check_payload_shape(&value)?;
        Ok(Self::new(kind, source, at, value))
    }

    /// The payload as `P`, or `None` when the event isn't of a kind `P` carries.
    pub fn decode<P: EventPayload>(&self) -> Option<Result<P, EventError>> {
        P::is_kind(self.kind.as_str())
            .then(|| P::deserialize(&self.payload).map_err(|e| EventError::from_serde(self.kind.as_str(), &e)))
    }

    /// With the platform's id for this event.
    #[must_use]
    pub fn with_origin_id(mut self, origin_id: impl Into<String>) -> Self {
        self.origin_id = Some(origin_id.into());
        self
    }

    /// With the platform's timestamp.
    #[must_use]
    pub fn with_utc(mut self, utc: UtcMillis) -> Self {
        self.utc = Some(utc);
        self
    }

    /// With this many `event.emit` hops. For the engine.
    #[must_use]
    pub fn with_hops(mut self, hops: u8) -> Self {
        self.hops = hops;
        self
    }

    /// Acting for `actor`. For `automation.fire` and the engine's `event.emit` only (design D7).
    #[must_use]
    pub fn with_acting(mut self, actor: Actor) -> Self {
        self.acting = Some(actor);
        self
    }
}

/// Checks the rules every payload keeps: an object, at most [`MAX_PAYLOAD_BYTES`] of JSON.
/// A writer that only counts what it is given.
struct ByteCount(usize);

impl std::io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn check_payload_shape(payload: &Value) -> Result<(), EventError> {
    if !payload.is_object() {
        return Err(EventError::NotAnObject);
    }
    // Counted as it serialises, without building the text: this runs on every publish.
    let mut counter = ByteCount(0);
    let bytes = serde_json::to_writer(&mut counter, payload).map_or(usize::MAX, |()| counter.0);
    if bytes > MAX_PAYLOAD_BYTES {
        return Err(EventError::TooLarge { bytes });
    }
    Ok(())
}

/// An event's arrival number, in the order the inbox took events.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct EventId(pub u64);

/// A wall-clock instant: milliseconds since the Unix epoch, UTC.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct UtcMillis(pub i64);

/// Who produced an event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Source {
    /// A platform adapter (BR-10) or a plugin adapter (PL-03) for a platform.
    #[non_exhaustive]
    Platform {
        /// The platform.
        platform: Platform,
        /// Whose channel (a collab may read several).
        channel: String,
    },
    /// The person's input source: hotkeys, MIDI, Stream Deck (PF-05, PF-25).
    Input,
    /// Voice commands (AP-10).
    Voice,
    /// OBS (BR-19).
    Obs,
    /// The engine: show lifecycle, `var.changed`, `engine.tick`.
    Engine,
    /// `event.emit` in a graph. For whom it acts is [`LiveEvent::acting`], not here.
    #[non_exhaustive]
    Graph {
        /// The emitting graph.
        graph: GraphId,
        /// The emitting node.
        node: NodeId,
    },
    /// `automation.fire` only (design D7): a test event that runs as `actor`.
    #[non_exhaustive]
    Manual {
        /// Who fired it; [`LiveEvent::acting`] carries the same actor.
        actor: Actor,
    },
    /// A plugin that isn't a platform adapter.
    #[non_exhaustive]
    Plugin {
        /// The plugin's id.
        id: String,
    },
    /// Test harnesses.
    Test,
}

impl Source {
    /// A platform adapter for `platform`, reading `channel`.
    pub fn platform(platform: Platform, channel: impl Into<String>) -> Self {
        Source::Platform { platform, channel: channel.into() }
    }

    /// `event.emit` in `graph`'s node `node`.
    pub fn graph(graph: GraphId, node: NodeId) -> Self {
        Source::Graph { graph, node }
    }

    /// `automation.fire` by `actor`.
    pub fn manual(actor: Actor) -> Self {
        Source::Manual { actor }
    }

    /// The plugin `id`.
    pub fn plugin(id: impl Into<String>) -> Self {
        Source::Plugin { id: id.into() }
    }
}

/// A typed event payload. `LiveEvent::of` and [`LiveEvent::decode`] convert; [`register_event_kind!`] registers it.
///
/// Payloads are additive (design D20): a new field is optional, a renamed one is a new kind. Every field is
/// documented, because its doc comment becomes the `event-<kind>--<field>` text.
pub trait EventPayload: Serialize + DeserializeOwned + JsonSchema {
    /// The kind this payload is published as.
    const KIND: &'static str;
    /// One line on what the event means: the `event-<kind>` text, the first line of the type's doc comment.
    const SUMMARY: &'static str;

    /// True for every kind this payload carries: [`KIND`](Self::KIND), and any other kind registered with the same
    /// payload (`show.started` and `show.stopping` share [`ShowLifecycle`](payloads::ShowLifecycle)).
    fn is_kind(kind: &str) -> bool {
        kind == Self::KIND
    }
}

/// Why a payload can't be made or read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EventError {
    /// The payload is larger than [`MAX_PAYLOAD_BYTES`] of JSON.
    #[non_exhaustive]
    #[error("a payload of {bytes} bytes is larger than the limit of {max} bytes", max = MAX_PAYLOAD_BYTES)]
    TooLarge {
        /// Its size as JSON.
        bytes: usize,
    },
    /// The payload isn't a JSON object.
    #[error("a payload is an object")]
    NotAnObject,
    /// The payload type doesn't carry this kind.
    #[non_exhaustive]
    #[error("a {payload} payload can't be published as {kind}")]
    WrongKind {
        /// The kind asked for.
        kind: String,
        /// The payload's own kind.
        payload: &'static str,
    },
    /// The payload doesn't match the kind's type.
    #[non_exhaustive]
    #[error("the payload doesn't fit {kind}: {reason}")]
    Decode {
        /// The kind.
        kind: String,
        /// What serde said.
        reason: String,
    },
}

impl EventError {
    /// A payload of `bytes` bytes of JSON, over [`MAX_PAYLOAD_BYTES`].
    pub fn too_large(bytes: usize) -> Self {
        EventError::TooLarge { bytes }
    }

    /// A `payload` payload asked to be published as `kind`.
    pub fn wrong_kind(kind: impl Into<String>, payload: &'static str) -> Self {
        EventError::WrongKind { kind: kind.into(), payload }
    }

    /// A payload that doesn't fit `kind`, for `reason`.
    pub fn decode(kind: impl Into<String>, reason: impl Into<String>) -> Self {
        EventError::Decode { kind: kind.into(), reason: reason.into() }
    }

    pub(crate) fn from_serde(kind: &str, error: &serde_json::Error) -> Self {
        Self::decode(kind, error.to_string())
    }
}

/// Where producers hand events over. Implemented by the engine's inbox (`omuzaki-live`) and by test sinks.
pub trait EventSink: Send + Sync {
    /// Hands `event` over. **Never blocks.**
    ///
    /// # Errors
    /// [`PublishError::Full`] for a droppable kind whose queue is full (the event is gone and counted) and for any
    /// other kind past the hard cap (the producer retains and retries, design D9); [`PublishError::ManualSource`] and
    /// [`PublishError::Acting`] for what only the engine may publish; [`PublishError::UnknownKind`],
    /// [`PublishError::BadPayload`] and [`PublishError::TooLarge`] for an event that doesn't fit its kind.
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError>;

    /// Reports that a producer's retry buffer of non-droppable events overflowed and dropped its `count` oldest
    /// (design D9). The engine adds them to `dropped.producer` and raises the "events are being lost" finding.
    /// **Never blocks.**
    fn producer_dropped(&self, count: u64);
}

/// Why [`EventSink::publish`] didn't take an event.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PublishError {
    /// The queue for this kind is full. Droppable: the event is gone. Otherwise: retry later, in order.
    #[error("the show can't keep up: its event queue is full")]
    Full,
    /// No kind of that name is registered (and it isn't `custom.*`).
    #[error("no event kind of that name is registered")]
    UnknownKind,
    /// The payload doesn't match the kind.
    #[non_exhaustive]
    #[error("the payload doesn't fit its kind: {error}")]
    BadPayload {
        /// Why.
        error: EventError,
    },
    /// The payload is larger than [`MAX_PAYLOAD_BYTES`].
    #[error("the payload is larger than the limit of {max} bytes", max = MAX_PAYLOAD_BYTES)]
    TooLarge,
    /// `Source::Manual` is made only by `automation.fire`.
    #[error("only automation.fire makes manual events")]
    ManualSource,
    /// `acting` is set only by `automation.fire` and the engine's `event.emit`.
    #[error("only the engine publishes events that act for a caller")]
    Acting,
}

impl PublishError {
    /// A payload that doesn't fit its kind.
    pub fn bad_payload(error: EventError) -> Self {
        match error {
            EventError::TooLarge { .. } => PublishError::TooLarge,
            error => PublishError::BadPayload { error },
        }
    }
}

/// Registers an event kind whose payload is `$ty` (ADR 0015): `register_event_kind!(Redeem)`, or
/// `register_event_kind!(ChatMessage, droppable)` for a kind the inbox may drop under load. A second kind that shares
/// a payload registers with `kind = "<kind>"` (`register_event_kind!(ShowLifecycle, kind = "show.stopping")`); its
/// payload's [`EventPayload::is_kind`] must accept it.
#[macro_export]
macro_rules! register_event_kind {
    (@submit $ty:ty, $kind:expr, $droppable:literal) => {
        // An invalid kind is a compile error, not a registration nobody can publish to.
        const _: $crate::ids::EventKind = $crate::ids::EventKind::from_static($kind);
        $crate::__inventory::submit! {
            $crate::event::EventKindRegistration::of::<$ty>($kind).droppable_if($droppable)
        }
        $crate::__record_registration!("event_kind", $ty);
    };
    ($ty:ty) => {
        $crate::register_event_kind!(@submit $ty, <$ty as $crate::event::EventPayload>::KIND, false);
    };
    ($ty:ty, droppable) => {
        $crate::register_event_kind!(@submit $ty, <$ty as $crate::event::EventPayload>::KIND, true);
    };
    ($ty:ty, kind = $kind:literal) => {
        $crate::register_event_kind!(@submit $ty, $kind, false);
    };
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn sources_are_tagged_by_kind() {
        let source = Source::graph(GraphId::new("bonk").unwrap(), NodeId::new("emit").unwrap());
        assert_eq!(serde_json::to_value(&source).unwrap(), json!({"kind": "graph", "graph": "bonk", "node": "emit"}));
        assert_eq!(serde_json::to_value(Source::Engine).unwrap(), json!({"kind": "engine"}));
        let manual = Source::manual(Actor::Person { name: "Kiri".into() });
        let back: Source = serde_json::from_value(serde_json::to_value(&manual).unwrap()).unwrap();
        assert_eq!(back, manual);
    }

    #[test]
    fn a_too_large_payload_is_refused_as_too_large() {
        assert_eq!(PublishError::bad_payload(EventError::TooLarge { bytes: 1 }), PublishError::TooLarge);
        assert!(check_payload_shape(&json!([1])).is_err());
    }
}
