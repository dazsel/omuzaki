//! The registry of event kinds (ADR 0015): what [`register_event_kind!`](crate::register_event_kind) submits, and the
//! lookups the inbox, the engine and the editor read.

use schemars::Schema;
use serde_json::Value;

use super::{EventError, EventPayload, PublishError, check_payload_shape};
use crate::ids::EventKind;

/// One registered event kind.
#[derive(Debug)]
#[non_exhaustive]
pub struct EventKindRegistration {
    /// The kind (`redeem`).
    pub kind: &'static str,
    /// One line on what it means, in English: the `event-<kind>` text.
    pub summary: &'static str,
    /// The payload's JSON Schema, inlined like op schemas: what `on.event` derives its ports from.
    pub schema: fn() -> Schema,
    /// Whether the inbox may drop it under load (design D8).
    pub droppable: bool,
    /// Checks a payload: decodes it into the typed payload.
    pub validate: fn(&Value) -> Result<(), EventError>,
}

impl EventKindRegistration {
    /// A kind with an untyped payload: any object passes [`validate`](Self::validate). For kinds registered without
    /// a Rust type; typed kinds use [`of`](Self::of).
    pub const fn new(kind: &'static str, summary: &'static str, schema: fn() -> Schema) -> Self {
        Self { kind, summary, schema, droppable: false, validate: any_object }
    }

    /// The kind `kind`, carrying `P`.
    pub const fn of<P: EventPayload>(kind: &'static str) -> Self {
        Self { kind, summary: P::SUMMARY, schema: schema_of::<P>, droppable: false, validate: validate_as::<P> }
    }

    /// Droppable under load.
    #[must_use]
    pub const fn droppable(self) -> Self {
        self.droppable_if(true)
    }

    /// Droppable under load when `droppable`.
    #[must_use]
    pub const fn droppable_if(mut self, droppable: bool) -> Self {
        self.droppable = droppable;
        self
    }

    /// With this payload check.
    #[must_use]
    pub const fn with_validate(mut self, validate: fn(&Value) -> Result<(), EventError>) -> Self {
        self.validate = validate;
        self
    }
}

/// The check of an untyped kind: the shape rules [`check_payload`] applies are all there is. The `Result` is the
/// shape every `validate` has.
#[allow(clippy::unnecessary_wraps)]
fn any_object(_: &Value) -> Result<(), EventError> {
    Ok(())
}

/// Inlined like op schemas: agents and the editor read flat schemas more reliably.
fn schema_of<P: EventPayload>() -> Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<P>()
}

fn validate_as<P: EventPayload>(payload: &Value) -> Result<(), EventError> {
    P::deserialize(payload).map(|_| ()).map_err(|e| EventError::decode(P::KIND, &e))
}

inventory::collect!(EventKindRegistration);

/// Every registered event kind, in link order. Surfaces sort by kind before showing them.
pub fn event_kinds() -> impl Iterator<Item = &'static EventKindRegistration> {
    inventory::iter::<EventKindRegistration>.into_iter()
}

/// The registration of `kind`. Linear: the inbox builds an index once.
pub fn find_event_kind(kind: &str) -> Option<&'static EventKindRegistration> {
    event_kinds().find(|r| r.kind == kind)
}

/// Checks a payload for `kind` as [`EventSink::publish`](super::EventSink::publish) must: an object within the size
/// limit, and for a registered kind one that its check accepts; `custom.*` takes any object.
///
/// # Errors
/// [`PublishError::TooLarge`], [`PublishError::BadPayload`] or [`PublishError::UnknownKind`].
pub fn check_payload(kind: &EventKind, payload: &Value) -> Result<(), PublishError> {
    check_payload_shape(payload).map_err(PublishError::bad_payload)?;
    if kind.is_custom() {
        return Ok(());
    }
    let registration = find_event_kind(kind.as_str()).ok_or(PublishError::UnknownKind)?;
    (registration.validate)(payload).map_err(PublishError::bad_payload)
}
