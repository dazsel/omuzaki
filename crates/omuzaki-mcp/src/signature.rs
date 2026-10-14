//! One-line call signatures from a JSON Schema, e.g. `face.set_profile(blend: number[0..1], eye?: {size: number})`.
//! They show up next to every `invalid` error so an agent can fix its call without fetching the schema again.

use rmcp::model::JsonObject;
use serde_json::{Map, Value};

/// How deep nested objects are spelt out before they are abbreviated to `object`.
const MAX_DEPTH: usize = 2;
/// Fields shown per object before `...`.
const MAX_FIELDS: usize = 8;
/// Alternatives shown for a union before `...`.
const MAX_VARIANTS: usize = 4;

/// `name(arg: type, optional?: type)`, required arguments first.
pub(crate) fn signature(name: &str, input: &JsonObject) -> String {
    let empty = Map::new();
    let properties = input.get("properties").and_then(Value::as_object).unwrap_or(&empty);
    let required: Vec<&str> =
        input.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    format!("{name}({})", fields(properties, &required, 0))
}

/// A short human type for one value: `string`, `number[0..1]`, `"a"|"b"`, `{x: number}`, `[number; 3]`, `any`.
pub(crate) fn shape(schema: &Value) -> String {
    shape_at(schema, 0)
}

fn required(schema: &Value) -> Vec<&str> {
    schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect()
}

fn shape_at(schema: &Value, depth: usize) -> String {
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        return capped(options.iter().map(Value::to_string), 8, "|");
    }
    if let Some(constant) = schema.get("const") {
        return constant.to_string();
    }
    match schema.get("type") {
        Some(Value::String(kind)) => typed(kind, schema, depth),
        Some(Value::Array(kinds)) => {
            kinds.iter().filter_map(Value::as_str).map(|kind| typed(kind, schema, depth)).collect::<Vec<_>>().join("|")
        }
        _ => union(schema, depth),
    }
}

fn typed(kind: &str, schema: &Value, depth: usize) -> String {
    match kind {
        "object" => object(schema, depth),
        "array" => array(schema, depth),
        other => format!("{other}{}", range_suffix(schema)),
    }
}

/// `{a: T, b?: U}` for objects with known fields (down to [`MAX_DEPTH`]), `{string: T}` for maps, else `object`.
fn object(schema: &Value, depth: usize) -> String {
    match schema.get("properties").and_then(Value::as_object) {
        Some(properties) if !properties.is_empty() && depth < MAX_DEPTH => {
            format!("{{{}}}", fields(properties, &required(schema), depth + 1))
        }
        Some(properties) if !properties.is_empty() => "object".to_owned(),
        _ => match schema.get("additionalProperties") {
            Some(values) if values.is_object() => format!("{{string: {}}}", shape_at(values, depth + 1)),
            _ => "object".to_owned(),
        },
    }
}

/// `[T]`, `[T; n]` for fixed length, `[A, B, C]` for tuples, else `array`.
fn array(schema: &Value, depth: usize) -> String {
    if let Some(Value::Array(items)) = schema.get("prefixItems") {
        return format!("[{}]", items.iter().map(|item| shape_at(item, depth + 1)).collect::<Vec<_>>().join(", "));
    }
    let Some(items) = schema.get("items").filter(|items| items.is_object()) else { return "array".to_owned() };
    let item = shape_at(items, depth + 1);
    match (schema.get("minItems").and_then(Value::as_u64), schema.get("maxItems").and_then(Value::as_u64)) {
        (Some(min), Some(max)) if min == max => format!("[{item}; {min}]"),
        _ => format!("[{item}]"),
    }
}

/// `anyOf`/`oneOf` as `a|b` (duplicates merged), or `any` when the schema says nothing usable.
fn union(schema: &Value, depth: usize) -> String {
    let variants = ["anyOf", "oneOf"].into_iter().find_map(|key| schema.get(key).and_then(Value::as_array));
    let Some(list) = variants.filter(|list| !list.is_empty()) else { return "any".to_owned() };
    let mut shapes: Vec<String> = Vec::new();
    for variant in list {
        let shape = shape_at(variant, depth);
        if !shapes.contains(&shape) {
            shapes.push(shape);
        }
    }
    capped(shapes.into_iter(), MAX_VARIANTS, "|")
}

/// Required fields first, then optional ones marked `?`.
fn fields(properties: &Map<String, Value>, required: &[&str], depth: usize) -> String {
    let (req, opt): (Vec<_>, Vec<_>) = properties.iter().partition(|(key, _)| required.contains(&key.as_str()));
    let parts = req.into_iter().chain(opt).map(|(key, schema)| {
        let mark = if required.contains(&key.as_str()) { "" } else { "?" };
        format!("{key}{mark}: {}", shape_at(schema, depth))
    });
    capped(parts, MAX_FIELDS, ", ")
}

/// Joins at most `max` items, then `...` if there were more.
fn capped(items: impl Iterator<Item = String>, max: usize, separator: &str) -> String {
    let mut shown: Vec<String> = items.take(max + 1).collect();
    if shown.len() > max {
        shown.truncate(max);
        shown.push("...".to_owned());
    }
    shown.join(separator)
}

/// `[min..max]` when the schema bounds a number, so the agent sees valid ranges up front.
fn range_suffix(schema: &Value) -> String {
    let bound = |key: &str| schema.get(key).and_then(Value::as_f64).map(|n| n.to_string());
    match (bound("minimum"), bound("maximum")) {
        (None, None) => String::new(),
        (lo, hi) => format!("[{}..{}]", lo.unwrap_or_default(), hi.unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn scalars_enums_and_ranges() {
        assert_eq!(shape(&json!({"type": "number", "minimum": 0.0, "maximum": 1.0})), "number[0..1]");
        assert_eq!(shape(&json!({"enum": ["soft", "hard"]})), "\"soft\"|\"hard\"");
        assert_eq!(shape(&json!({"type": ["string", "null"]})), "string|null");
        assert_eq!(shape(&json!({})), "any");
    }

    #[test]
    fn nested_shapes_are_spelt_out_not_called_object() {
        let eye = json!({
            "type": ["object", "null"],
            "properties": {"size": {"type": "number"}, "color": {"type": "array", "items": {"type": "number"},
                "minItems": 3, "maxItems": 3}},
            "required": ["size"]
        });
        assert_eq!(shape(&eye), "{size: number, color?: [number; 3]}|null");
        let parts = json!({"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}});
        assert_eq!(shape(&parts), "[{id: string}]");
        let map = json!({"type": "object", "additionalProperties": {"type": "number"}});
        assert_eq!(shape(&map), "{string: number}");
        let pair = json!({"type": "array", "prefixItems": [{"type": "number"}, {"type": "string"}]});
        assert_eq!(shape(&pair), "[number, string]");
    }

    #[test]
    fn tagged_unions_list_each_variant_once() {
        let union = json!({"oneOf": [
            {"type": "object", "properties": {"kind": {"const": "round"}, "r": {"type": "number"}}, "required": ["kind", "r"]},
            {"type": "object", "properties": {"kind": {"const": "square"}}, "required": ["kind"]},
            {"type": "object", "properties": {"kind": {"const": "square"}}, "required": ["kind"]}
        ]});
        assert_eq!(shape(&union), "{kind: \"round\", r: number}|{kind: \"square\"}");
    }

    #[test]
    fn deep_nesting_is_abbreviated() {
        let deep = json!({"type": "object", "properties": {"a": {"type": "object", "properties": {"b": {"type": "object",
            "properties": {"c": {"type": "number"}}}}}}});
        assert_eq!(shape(&deep), "{a?: {b?: object}}");
    }

    #[test]
    fn signatures_put_required_arguments_first() {
        let input =
            json!({"properties": {"mode": {"enum": ["a"]}, "blend": {"type": "number"}}, "required": ["blend"]});
        assert_eq!(
            signature("face.set_blend", input.as_object().unwrap()),
            "face.set_blend(blend: number, mode?: \"a\")"
        );
        assert_eq!(signature("project.ping", &JsonObject::new()), "project.ping()");
    }

    #[test]
    fn long_field_lists_are_capped() {
        let properties: Map<String, Value> = (0..12).map(|i| (format!("f{i:02}"), json!({"type": "string"}))).collect();
        let text = signature("x.y", json!({"properties": properties}).as_object().unwrap());
        assert!(text.ends_with("f07?: string, ...)"), "{text}");
    }
}
