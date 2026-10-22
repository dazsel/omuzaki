//! Reading port types out of JSON Schemas: an event kind's payload schema for `on.event`'s outputs, an op's argument
//! schema for `op.run`'s placeholders. Both schemas are inlined (no `$ref`), as their registries generate them.

use omuzaki_live_api::value::PortType;
use serde_json::Value;

/// The schema itself, or for an optional field (`anyOf`/`oneOf` with `null`) the one schema that isn't `null`.
pub(crate) fn resolve(schema: &Value) -> &Value {
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(options)) = schema.get(key) {
            let mut real = options.iter().filter(|s| type_name(s) != Some("null"));
            if let (Some(one), None) = (real.next(), real.next()) {
                return resolve(one);
            }
        }
    }
    schema
}

/// The schema's own type name: `type` (the first that isn't `null` when it is a list), else `object` for a schema
/// with properties and `string` for an enum of strings.
pub(crate) fn type_name(schema: &Value) -> Option<&str> {
    match schema.get("type") {
        Some(Value::String(name)) => return Some(name),
        Some(Value::Array(names)) => return names.iter().filter_map(Value::as_str).find(|n| *n != "null"),
        _ => {}
    }
    if schema.get("properties").is_some() {
        return Some("object");
    }
    match schema.get("enum").or_else(|| schema.get("const")) {
        Some(Value::Array(values)) if values.iter().all(Value::is_string) => return Some("string"),
        Some(Value::String(_)) => return Some("string"),
        _ => {}
    }
    // A choice whose options all have one type (an enum with documented variants is a `oneOf` of `const`s).
    ["oneOf", "anyOf"].iter().find_map(|key| {
        let options = schema.get(*key)?.as_array()?;
        let mut names = options.iter().map(|s| type_name(resolve(s))).filter(|n| *n != Some("null"));
        let first = names.next()??;
        names.all(|n| n == Some(first)).then_some(first)
    })
}

/// The port type a value of this schema takes; objects and anything unknown are `json`.
pub(crate) fn port_type(schema: &Value) -> PortType {
    match type_name(resolve(schema)) {
        Some("string") => PortType::Text,
        Some("number" | "integer") => PortType::Number,
        Some("boolean") => PortType::Bool,
        Some("array") => PortType::List,
        _ => PortType::Json,
    }
}

/// The schema's properties, when it is an object schema that lists them.
pub(crate) fn properties(schema: &Value) -> Option<&serde_json::Map<String, Value>> {
    resolve(schema).get("properties").and_then(Value::as_object)
}

/// Whether the object schema refuses properties it doesn't list (`additionalProperties: false`, which serde's
/// `deny_unknown_fields` produces).
pub(crate) fn closed(schema: &Value) -> bool {
    resolve(schema).get("additionalProperties") == Some(&Value::Bool(false))
}
