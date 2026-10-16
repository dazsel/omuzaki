//! The JSON Schema subset a plugin setting's `schema` may use, and the check of a value against it (design §7,
//! `[[settings]]`). A manifest's default is checked here, and the settings work checks stored values the same way.
//!
//! The subset: `type` (one name or a list), `enum`, `const`, `minimum`, `maximum`, `exclusiveMinimum`,
//! `exclusiveMaximum`, `minLength`, `maxLength` (in characters), `items` (one schema), `minItems`, `maxItems`,
//! `properties`, `required`, `additionalProperties` (a bool or a schema), and the annotations `title`,
//! `description`, `default`, `examples` and `$comment`. Any other keyword is refused rather than ignored, so a schema
//! never promises a check that does not happen.

use std::cmp::Ordering;

use serde_json::{Map, Number, Value};

/// How deep a schema may nest.
pub const MAX_SCHEMA_DEPTH: usize = 16;

/// The keywords that only describe, and check nothing.
const ANNOTATIONS: [&str; 5] = ["title", "description", "default", "examples", "$comment"];

/// The keywords that check.
const CHECKS: [&str; 15] = [
    "type",
    "enum",
    "const",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "items",
    "minItems",
    "maxItems",
    "properties",
    "required",
    "additionalProperties",
];

/// Why `schema` is not one the subset accepts: an unknown keyword, a keyword of the wrong shape, or nesting past
/// [`MAX_SCHEMA_DEPTH`].
pub fn schema_problem(schema: &Value) -> Option<String> {
    shape(schema, 0).err()
}

/// Why `value` does not satisfy `schema`, or `None` when it does. `schema` is one [`schema_problem`] accepts.
pub fn value_problem(schema: &Value, value: &Value) -> Option<String> {
    let Value::Object(schema) = schema else { return Some("the schema is not an object".into()) };
    check(schema, value, "").err()
}

fn shape(schema: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("nests deeper than {MAX_SCHEMA_DEPTH}"));
    }
    let Value::Object(schema) = schema else { return Err("a schema must be an object".into()) };
    for (key, value) in schema {
        if ANNOTATIONS.contains(&key.as_str()) {
            continue;
        }
        if !CHECKS.contains(&key.as_str()) {
            return Err(format!("uses `{key}`, which plugin settings don't support"));
        }
        let ok = match key.as_str() {
            "type" => match value {
                Value::String(name) => type_name_ok(name),
                Value::Array(names) => names.iter().all(|n| n.as_str().is_some_and(type_name_ok)),
                _ => false,
            },
            "enum" => value.is_array(),
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => value.is_number(),
            "minLength" | "maxLength" | "minItems" | "maxItems" => value.is_u64(),
            "required" => value.as_array().is_some_and(|keys| keys.iter().all(Value::is_string)),
            "items" => {
                shape(value, depth + 1)?;
                true
            }
            "properties" => {
                let Value::Object(properties) = value else { return Err("`properties` must be an object".into()) };
                for property in properties.values() {
                    shape(property, depth + 1)?;
                }
                true
            }
            "additionalProperties" => value.is_boolean() || shape(value, depth + 1).is_ok(),
            _ => true, // `const` takes any value.
        };
        if !ok {
            return Err(format!("`{key}` has the wrong shape"));
        }
    }
    Ok(())
}

fn type_name_ok(name: &str) -> bool {
    matches!(name, "null" | "boolean" | "integer" | "number" | "string" | "array" | "object")
}

fn type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if is_whole(n) => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `at` is the value's JSON Pointer under the default, for the message.
fn check(schema: &Map<String, Value>, value: &Value, at: &str) -> Result<(), String> {
    let here = if at.is_empty() { "the value".to_owned() } else { format!("`{at}`") };
    if let Some(kind) = schema.get("type") {
        let actual = type_of(value);
        let fits = |name: &str| name == actual || (name == "number" && actual == "integer");
        let ok = match kind {
            Value::String(name) => fits(name),
            Value::Array(names) => names.iter().filter_map(Value::as_str).any(fits),
            _ => false,
        };
        if !ok {
            return Err(format!("{here} is a {actual}, not {kind}"));
        }
    }
    if let Some(Value::Array(allowed)) = schema.get("enum")
        && !allowed.contains(value)
    {
        return Err(format!("{here} is not one of {}", Value::Array(allowed.clone())));
    }
    if let Some(expected) = schema.get("const")
        && expected != value
    {
        return Err(format!("{here} is not {expected}"));
    }
    if let Value::Number(number) = value {
        bounds(schema, number, &here)?;
    }
    if let Some(text) = value.as_str() {
        let chars = text.chars().count() as u64;
        limit(schema, "minLength", "maxLength", chars, &here, "characters")?;
    }
    if let Some(items) = value.as_array() {
        limit(schema, "minItems", "maxItems", items.len() as u64, &here, "items")?;
        if let Some(Value::Object(item)) = schema.get("items") {
            for (i, element) in items.iter().enumerate() {
                check(item, element, &format!("{at}/{i}"))?;
            }
        }
    }
    if let Some(object) = value.as_object() {
        object_checks(schema, object, at, &here)?;
    }
    Ok(())
}

fn bounds(schema: &Map<String, Value>, number: &Number, here: &str) -> Result<(), String> {
    let compared = |key: &str| schema.get(key).and_then(Value::as_number).and_then(|bound| compare(number, bound));
    let is = |key: &str, order: &[Ordering]| compared(key).is_some_and(|o| order.contains(&o));
    if is("minimum", &[Ordering::Less]) || is("exclusiveMinimum", &[Ordering::Less, Ordering::Equal]) {
        return Err(format!("{here} is below the minimum"));
    }
    if is("maximum", &[Ordering::Greater]) || is("exclusiveMaximum", &[Ordering::Greater, Ordering::Equal]) {
        return Err(format!("{here} is above the maximum"));
    }
    Ok(())
}

/// Whether `n` is a whole number, as JSON Schema counts one: an integer, or a float with no fraction (TOML's `3.0`).
pub fn is_whole(n: &Number) -> bool {
    n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0)
}

/// A JSON number exactly: an integer as it was written (past 2⁵³ too), else its float.
enum Exact {
    Int(i128),
    Float(f64),
}

fn exact(n: &Number) -> Option<Exact> {
    if let Some(i) = n.as_i64() {
        Some(Exact::Int(i128::from(i)))
    } else if let Some(u) = n.as_u64() {
        Some(Exact::Int(i128::from(u)))
    } else {
        n.as_f64().map(Exact::Float)
    }
}

/// `a` against `b` without rounding an integer through a float.
fn compare(a: &Number, b: &Number) -> Option<Ordering> {
    match (exact(a)?, exact(b)?) {
        (Exact::Int(x), Exact::Int(y)) => Some(x.cmp(&y)),
        (Exact::Float(x), Exact::Float(y)) => x.partial_cmp(&y),
        (Exact::Int(x), Exact::Float(y)) => int_against_float(x, y),
        (Exact::Float(x), Exact::Int(y)) => int_against_float(y, x).map(Ordering::reverse),
    }
}

/// 2¹²⁶: a float this far from zero is beyond every i64 and u64, so an integer is on the near side of it.
const FLOAT_PAST_INTEGERS: f64 = 8.507_059_173_023_462e37;

/// An integer against a float, exactly: against the float's whole part, and below it when the float has a fraction.
fn int_against_float(int: i128, float: f64) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    if float >= FLOAT_PAST_INTEGERS {
        return Some(Ordering::Less);
    }
    if float <= -FLOAT_PAST_INTEGERS {
        return Some(Ordering::Greater);
    }
    let floor = float.floor();
    // In range, so the cast is exact.
    #[allow(clippy::cast_possible_truncation)]
    let whole = floor as i128;
    Some(match int.cmp(&whole) {
        Ordering::Equal if floor < float => Ordering::Less,
        order => order,
    })
}

fn limit(schema: &Map<String, Value>, min: &str, max: &str, count: u64, here: &str, unit: &str) -> Result<(), String> {
    if let Some(least) = schema.get(min).and_then(Value::as_u64)
        && count < least
    {
        return Err(format!("{here} has {count} {unit}, under `{min}` {least}"));
    }
    if let Some(most) = schema.get(max).and_then(Value::as_u64)
        && count > most
    {
        return Err(format!("{here} has {count} {unit}, over `{max}` {most}"));
    }
    Ok(())
}

fn object_checks(schema: &Map<String, Value>, object: &Map<String, Value>, at: &str, here: &str) -> Result<(), String> {
    if let Some(Value::Array(required)) = schema.get("required")
        && let Some(missing) = required.iter().filter_map(Value::as_str).find(|k| !object.contains_key(*k))
    {
        return Err(format!("{here} lacks the required `{missing}`"));
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    for (key, element) in object {
        let path = format!("{at}/{key}");
        match properties.and_then(|p| p.get(key)) {
            Some(Value::Object(property)) => check(property, element, &path)?,
            Some(_) => {}
            None => match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => return Err(format!("{here} has `{key}`, which the schema doesn't allow")),
                Some(Value::Object(extra)) => check(extra, element, &path)?,
                _ => {}
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_subset_checks_types_bounds_lengths_items_and_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "volume": {"type": "number", "minimum": 0, "maximum": 1},
                "names": {"type": "array", "items": {"type": "string", "maxLength": 3}, "maxItems": 2},
                "mode": {"enum": ["a", "b"]}
            },
            "required": ["volume"],
            "additionalProperties": false
        });
        assert_eq!(schema_problem(&schema), None);
        assert_eq!(value_problem(&schema, &json!({"volume": 0.5, "names": ["abc"], "mode": "a"})), None);
        let bad = [
            json!({"names": []}),
            json!({"volume": 2}),
            json!({"volume": 0, "names": ["abcd"]}),
            json!({"volume": 0, "names": ["a", "b", "c"]}),
            json!({"volume": 0, "mode": "c"}),
            json!({"volume": 0, "other": 1}),
            json!({"volume": "loud"}),
        ];
        for value in bad {
            assert!(value_problem(&schema, &value).is_some(), "{value}");
        }
        assert_eq!(value_problem(&json!({"type": "number"}), &json!(3)), None, "an integer is a number");
        assert_eq!(value_problem(&json!({"type": "integer"}), &json!(3.0)), None, "TOML's 3.0 is a whole number");
        assert!(value_problem(&json!({"type": "integer"}), &json!(3.5)).is_some());
    }

    #[test]
    fn integer_bounds_past_two_to_the_53_compare_exactly() {
        let big = 9_007_199_254_740_993_u64; // 2^53 + 1: as an f64 it would round to 2^53
        let schema = json!({"type": "integer", "maximum": 9_007_199_254_740_992_u64});
        assert!(value_problem(&schema, &json!(big)).is_some(), "one above the maximum is above it");
        assert_eq!(value_problem(&schema, &json!(big - 1)), None);
        let fractional = json!({"exclusiveMaximum": 2.5, "minimum": 1.5});
        assert_eq!(value_problem(&fractional, &json!(2)), None);
        assert!(value_problem(&fractional, &json!(3)).is_some() && value_problem(&fractional, &json!(1)).is_some());
    }

    #[test]
    fn a_keyword_outside_the_subset_or_too_deep_a_schema_is_refused() {
        assert_eq!(
            schema_problem(&json!({"type": "string", "pattern": "^a"})),
            Some("uses `pattern`, which plugin settings don't support".into())
        );
        assert!(schema_problem(&json!({"items": {"$ref": "#"}})).is_some());
        assert!(schema_problem(&json!({"type": "colour"})).is_some());
        let mut deep = json!({"type": "array"});
        for _ in 0..=MAX_SCHEMA_DEPTH {
            deep = json!({"items": deep});
        }
        assert_eq!(schema_problem(&deep), Some(format!("nests deeper than {MAX_SCHEMA_DEPTH}")));
    }
}
