//! `on.event`: the trigger on any event kind (design §6, BR-11's "event → condition → action").
//!
//! Its value ports come from the kind's payload schema, deterministically: each top-level property that is text, a
//! number, true/false or a list becomes a port of that type; an object property gives its own fields one level down
//! as `<property>_<field>` (a redeem gives `user_name`, `reward_title`, `reward_cost`, ...), and anything deeper is a
//! json port. The value ports are sorted by name, then come `event` (the whole payload) and `fired`. A missing
//! optional field reads as its type's default.
//!
//! `where` compares a dotted path to a literal (text through [`fold`](crate::fold), so case and width don't matter; a
//! list matches when any item does), and `min` keeps events whose number at a path is at least the given value.

use std::collections::BTreeMap;

use omuzaki_live_api::event::find_event_kind;
use omuzaki_live_api::ids::{EventKind, PortName};
use omuzaki_live_api::node::{
    Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors, Ports,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fold::same;
use crate::l10n;
use crate::schema::{port_type, properties, resolve, type_name};

/// The output that carries the whole payload.
const EVENT_PORT: &str = "event";
/// The output that fires on a match.
const FIRED_PORT: &str = "fired";

/// Parameters of `on.event`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OnEventParams {
    /// The event kind it listens to: `redeem`, `chat.message`, `follow`, `raid`, `cheer`, `custom.<name>`, ... (the
    /// kinds `automation.event_kinds` lists).
    pub kind: String,
    /// Only events whose payload holds these values: a dotted path (`reward.title`) to text, a number or true/false.
    /// Text compares ignoring case and width; a list matches when any of its items does.
    #[serde(default, rename = "where", skip_serializing_if = "BTreeMap::is_empty")]
    pub filter: BTreeMap<String, Value>,
    /// Only events whose payload numbers are at least these: a dotted path (`bits`) to the least value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub min: BTreeMap<String, f64>,
}

impl Default for OnEventParams {
    fn default() -> Self {
        Self { kind: "custom.event".to_owned(), filter: BTreeMap::new(), min: BTreeMap::new() }
    }
}

/// One value output: its port, type and where in the payload it reads from.
#[derive(Clone, Debug, PartialEq)]
struct Field {
    port: PortName,
    ty: PortType,
    path: Vec<String>,
}

/// The value outputs a kind's payload schema gives, sorted by port name. Empty for `custom.*` kinds, whose payloads
/// have no schema, and for a kind nothing registered.
fn fields(kind: &str) -> Vec<Field> {
    let Some(registration) = find_event_kind(kind) else {
        return Vec::new();
    };
    let schema = (registration.schema)();
    let Some(props) = properties(schema.as_value()) else {
        return Vec::new();
    };
    let mut fields = BTreeMap::new();
    let mut add = |name: String, ty: PortType, path: Vec<String>| {
        if let Ok(port) = PortName::new(name)
            && port.as_str() != EVENT_PORT
            && port.as_str() != FIRED_PORT
        {
            fields.entry(port.clone()).or_insert(Field { port, ty, path });
        }
    };
    for (name, prop) in props {
        match properties(prop) {
            Some(inner) if type_name(resolve(prop)) == Some("object") => {
                for (field, sub) in inner {
                    add(format!("{name}_{field}"), port_type(sub), vec![name.clone(), field.clone()]);
                }
            }
            _ => add(name.clone(), port_type(prop), vec![name.clone()]),
        }
    }
    fields.into_values().collect()
}

/// The value at a dotted path, if every step exists.
fn lookup<'a>(payload: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(payload, |value, key| value.get(key))
}

/// A dotted path as its steps.
fn steps(path: &str) -> Vec<String> {
    path.split('.').map(str::to_owned).collect()
}

/// Whether a payload value matches a `where` literal.
fn matches(found: &Value, expected: &Value) -> bool {
    match (found, expected) {
        (Value::Array(items), _) => items.iter().any(|item| matches(item, expected)),
        (Value::String(a), Value::String(b)) => same(a, b),
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Bool(a), Value::Bool(b)) => a == b,
        _ => false,
    }
}

/// Fires `fired` for each event of its kind that passes the filters, with the payload's fields on its value ports.
#[derive(Debug)]
pub struct OnEvent {
    fields: Vec<Field>,
    filter: Vec<(Vec<String>, Value)>,
    min: Vec<(Vec<String>, f64)>,
}

impl OnEvent {
    fn passes(&self, payload: &Value) -> bool {
        self.filter.iter().all(|(path, expected)| lookup(payload, path).is_some_and(|found| matches(found, expected)))
            && self
                .min
                .iter()
                .all(|(path, least)| lookup(payload, path).and_then(Value::as_f64).is_some_and(|x| x >= *least))
    }

    fn on_trigger(&self, payload: &Value, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if !self.passes(payload) {
            return Ok(());
        }
        for field in &self.fields {
            let value = lookup(payload, &field.path)
                .filter(|v| !v.is_null())
                .and_then(|v| Val::from_json(v, field.ty).ok())
                .or_else(|| field.ty.default_value());
            if let Some(value) = value {
                cx.set(field.port.as_str(), value)?;
            }
        }
        cx.set(EVENT_PORT, Val::from(payload.clone()))?;
        cx.fire(FIRED_PORT)
    }
}

impl NodeKind for OnEvent {
    type Params = OnEventParams;

    fn meta() -> NodeMeta {
        NodeMeta::new(
            "on.event",
            "When an event of a kind arrives, and it passes the filters.",
            NodeClass::Trigger,
            Category::Triggers,
        )
    }

    fn ports(params: &OnEventParams) -> Ports {
        let ports = fields(&params.kind)
            .into_iter()
            .fold(Ports::new(), |ports, field| ports.dynamic_out(field.port, field.ty, field.path.join(".")));
        ports
            .value_out(EVENT_PORT, PortType::Json, "The whole event payload.")
            .event_out(FIRED_PORT, "An event of the kind arrived and passed the filters.")
    }

    fn listens(params: &OnEventParams) -> Vec<EventKind> {
        EventKind::new(params.kind.as_str()).map(|kind| vec![kind]).unwrap_or_default()
    }

    fn check(params: &OnEventParams) -> Result<(), ParamErrors> {
        let kind = EventKind::new(params.kind.as_str())
            .map_err(|e| ParamErrors::one("kind", l10n::bad_event_kind(params.kind.as_str(), e.to_string())))?;
        if !kind.is_custom() && find_event_kind(kind.as_str()).is_none() {
            return Err(ParamErrors::one("kind", l10n::unknown_event_kind(kind.as_str())));
        }
        let mut errors = Vec::new();
        for (path, value) in &params.filter {
            if !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                errors.push(ParamError::new(format!("where.{path}"), l10n::filter_not_scalar(path.as_str())));
            }
        }
        for (path, least) in &params.min {
            if !least.is_finite() {
                errors.push(ParamError::new(format!("min.{path}"), l10n::min_not_finite(path.as_str())));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(ParamErrors(errors)) }
    }

    fn new(params: OnEventParams) -> Self {
        Self {
            fields: fields(&params.kind),
            filter: params.filter.into_iter().map(|(path, value)| (steps(&path), value)).collect(),
            min: params.min.into_iter().map(|(path, least)| (steps(&path), least)).collect(),
        }
    }
}

impl Node for OnEvent {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Trigger { event, .. } => self.on_trigger(&event.payload, cx),
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(OnEvent);
