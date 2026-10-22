//! `event.emit`: sends a `custom.<name>` event into the running show, so one graph can start another (design §6, D9).
//! The event carries the emitting token's hop count plus one, and the loop guard suspends a graph whose events keep
//! emitting each other past [`MAX_EMIT_HOPS`](omuzaki_live_api::limits::MAX_EMIT_HOPS).

use std::sync::Arc;

use omuzaki_live_api::ids::EventKind;
use omuzaki_live_api::node::{
    Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamErrors, Ports,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::l10n;

/// Parameters of `event.emit`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmitParams {
    /// The kind it sends: `custom.<name>`, which an `on.event` with the same kind hears.
    pub kind: String,
}

impl Default for EmitParams {
    fn default() -> Self {
        Self { kind: "custom.event".to_owned() }
    }
}

/// The kind, when it is a valid `custom.<name>`.
fn custom_kind(kind: &str) -> Option<EventKind> {
    EventKind::new(kind).ok().filter(EventKind::is_custom)
}

/// What a payload that isn't an object is, for the error.
fn found(value: &Val) -> &'static str {
    match value {
        Val::Json(json) => match json.as_ref() {
            Value::Array(_) => "a list",
            Value::String(_) => "text",
            Value::Number(_) => "a number",
            Value::Bool(_) => "true or false",
            Value::Null | Value::Object(_) => "json",
        },
        other => other.ty().as_str(),
    }
}

/// Emits its event each time `in` fires, then fires `done`, or `failed` with the reason.
#[derive(Debug)]
pub struct Emit {
    kind: Option<EventKind>,
}

impl Emit {
    fn on_in(&mut self, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        let Some(kind) = self.kind.clone() else {
            return cx.fail("failed", l10n::emit_not_custom(""));
        };
        let payload = match cx.input("payload")? {
            Val::Json(value) if value.is_object() => Arc::unwrap_or_clone(value),
            Val::Json(value) if value.is_null() => Value::Object(Map::new()),
            other => return cx.fail("failed", l10n::emit_not_object(found(&other))),
        };
        match cx.emit(kind, payload) {
            Ok(()) => cx.fire("done"),
            Err(e) => cx.fail("failed", l10n::emit_refused(e.to_string())),
        }
    }
}

impl NodeKind for Emit {
    type Params = EmitParams;

    fn meta() -> NodeMeta {
        NodeMeta::new(
            "event.emit",
            "Send a custom event that other automations can listen to.",
            NodeClass::Active,
            Category::Events,
        )
    }

    fn ports(_: &EmitParams) -> Ports {
        Ports::new()
            .event_in("in", "Sends the event.")
            .value_in(
                "payload",
                PortType::Json,
                Val::Json(Arc::new(Value::Object(Map::new()))),
                "The event's payload: an object.",
            )
            .event_out("done", "The event was sent.")
            .event_out("failed", "The event wasn't sent; the reason is on `error`.")
            .value_out("error", PortType::Text, "Why it wasn't sent, in English.")
    }

    fn check(params: &EmitParams) -> Result<(), ParamErrors> {
        match custom_kind(&params.kind) {
            Some(_) => Ok(()),
            None => Err(ParamErrors::one("kind", l10n::emit_not_custom(params.kind.as_str()))),
        }
    }

    fn new(params: EmitParams) -> Self {
        Self { kind: custom_kind(&params.kind) }
    }
}

impl Node for Emit {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => self.on_in(cx),
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Emit);
