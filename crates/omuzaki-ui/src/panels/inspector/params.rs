//! A part's parameters, as `part.describe` lists them, turned into the JSON Schema and the value `ds::schema_form`
//! draws.
//!
//! `part.describe` answers with the kind's schema and with `parameters`: every node of the part's parameters, parents
//! first, each with its `/`-separated path, its type, and for a value its default, its current value and what it
//! accepts (the range is probed from the kind's own check, so it is exact even though the kind's schema declares none,
//! and a map's entries and a list's items are listed from the part's values). The form is built from `parameters`, so:
//!
//! - every number has the bounds the kind enforces, and gets a slider when it has both;
//! - a map entry or a list item is a section or field of its own, where the kind's schema (a map, a tagged union) would
//!   be a read-only row;
//! - a field's JSON Pointer is its `part.set_param` path with a leading `/`, so an edit needs no translation.

use omuzaki_doc_api::Value;
use serde_json::{Map, json};

/// The fields of the form and their current values, read from a `part.describe` answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// The JSON Schema of the form: one object whose properties mirror the parameter paths.
    pub schema: Value,
    /// The current values in the same shape.
    pub value: Value,
    /// How many fields (values, not sections) the form has.
    pub fields: usize,
}

/// The form of a `part.describe` answer, keeping only the parameters whose path or description contains `query`
/// (ignoring case) and the sections above them. An empty query keeps everything.
pub fn read(answer: &Value, query: &str) -> Params {
    let mut schema = object_schema(None);
    let mut value = Value::Object(Map::new());
    let mut fields = 0;
    let entries = answer.get("parameters").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let query = query.trim().to_lowercase();
    for entry in entries {
        let Some(path) = entry.get("path").and_then(Value::as_str) else { continue };
        let ty = entry.get("type").and_then(Value::as_str).unwrap_or("any");
        if is_container(ty, entry) {
            continue;
        }
        if !query.is_empty() && !matches(entry, path, &query) {
            continue;
        }
        let steps: Vec<&str> = path.split('/').collect();
        let Some((last, parents)) = steps.split_last() else { continue };
        let (Some(section), Some(holder)) = (section_at(&mut schema, parents, entries), value_at(&mut value, parents))
        else {
            continue;
        };
        if let Some(props) = section.get_mut("properties").and_then(Value::as_object_mut) {
            props.insert((*last).to_owned(), field_schema(entry, ty, last, parents.last().copied()));
        }
        if let Some(current) = entry.get("current") {
            holder.insert((*last).to_owned(), current.clone());
        }
        fields += 1;
    }
    Params { schema, value, fields }
}

/// Whether the entry is a section, map or list whose members follow it (it has no value of its own then).
fn is_container(ty: &str, entry: &Value) -> bool {
    matches!(ty, "section" | "map" | "list") && entry.get("current").is_none()
}

fn matches(entry: &Value, path: &str, query: &str) -> bool {
    path.to_lowercase().replace(['_', '/'], " ").contains(query)
        || entry.get("description").and_then(Value::as_str).is_some_and(|d| d.to_lowercase().contains(query))
}

/// The schema of a section, titled when it has a name.
fn object_schema(title: Option<&str>) -> Value {
    let mut schema = json!({"type": "object", "properties": {}});
    if let (Some(title), Some(map)) = (title, schema.as_object_mut()) {
        map.insert("title".to_owned(), Value::String(title.to_owned()));
    }
    schema
}

/// The section schema for `parents`, made on the way down. Each section's description comes from its own entry.
fn section_at<'a>(schema: &'a mut Value, parents: &[&str], entries: &[Value]) -> Option<&'a mut Map<String, Value>> {
    let mut at = schema;
    let mut path = String::new();
    for step in parents {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(step);
        let props = at.get_mut("properties")?.as_object_mut()?;
        let section = props.entry((*step).to_owned()).or_insert_with(|| {
            let mut section = object_schema(Some(&title_of(step)));
            let described = entries.iter().find(|e| e.get("path").and_then(Value::as_str) == Some(path.as_str()));
            if let (Some(description), Some(map)) =
                (described.and_then(|e| e.get("description")), section.as_object_mut())
            {
                map.insert("description".to_owned(), description.clone());
            }
            section
        });
        at = section;
    }
    at.as_object_mut()
}

/// The value object for `parents`, made on the way down.
fn value_at<'a>(value: &'a mut Value, parents: &[&str]) -> Option<&'a mut Map<String, Value>> {
    let mut at = value;
    for step in parents {
        at = at.as_object_mut()?.entry((*step).to_owned()).or_insert_with(|| Value::Object(Map::new()));
    }
    at.as_object_mut()
}

/// The schema of one value, `key` in the section or list `parent`.
fn field_schema(entry: &Value, ty: &str, key: &str, parent: Option<&str>) -> Value {
    let mut field = Map::new();
    field.insert("title".to_owned(), Value::String(item_title(key, parent)));
    let copy = |field: &mut Map<String, Value>, keys: &[&str]| {
        for k in keys {
            if let Some(v) = entry.get(*k) {
                field.insert((*k).to_owned(), v.clone());
            }
        }
    };
    match ty {
        "number" | "integer" => {
            field.insert("type".to_owned(), Value::String(ty.to_owned()));
            copy(&mut field, &["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"]);
        }
        "boolean" => {
            field.insert("type".to_owned(), json!("boolean"));
        }
        "text" => {
            field.insert("type".to_owned(), json!("string"));
            copy(&mut field, &["pattern"]);
        }
        "choice" => {
            field.insert("enum".to_owned(), entry.get("values").cloned().unwrap_or_else(|| json!([])));
        }
        // A list or map set whole, or a value the schema doesn't describe: shown, not edited here.
        other => {
            field.insert("type".to_owned(), Value::String(if other == "list" { "array" } else { other }.to_owned()));
        }
    }
    copy(&mut field, &["description", "default"]);
    if let Some(unit) = entry.get("unit") {
        field.insert("x-ui".to_owned(), json!({"unit": unit}));
    }
    Value::Object(field)
}

/// A field's label. A list item is named by its list and its place from 1 (`centre/0` reads "Centre 1"): an index
/// alone would be read out as a bare "0", the same in every list of the part.
fn item_title(key: &str, parent: Option<&str>) -> String {
    match (key.parse::<usize>(), parent) {
        (Ok(index), Some(list)) => format!("{} {}", title_of(list), index.saturating_add(1)),
        _ => title_of(key),
    }
}

/// A key as a label: `half_width` reads "Half width". A list index stays a number.
fn title_of(key: &str) -> String {
    let mut words = key.replace(['_', '-'], " ");
    if let Some(first) = words.get(..1) {
        let upper = first.to_uppercase();
        words.replace_range(..1, &upper);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer() -> Value {
        json!({"parameters": [
            {"path": "cranium", "type": "section", "description": "The cranium."},
            {"path": "cranium/half_width", "type": "number", "minimum": 0.05, "maximum": 0.4, "default": 0.186,
             "current": 0.2, "unit": "m", "description": "Half the width."},
            {"path": "cranium/centre", "type": "list"},
            {"path": "cranium/centre/0", "type": "number", "minimum": -4.0, "maximum": 4.0, "current": 0.0},
            {"path": "groups", "type": "map"},
            {"path": "groups/a-cap", "type": "section"},
            {"path": "groups/a-cap/dynamics/follow", "type": "choice", "values": ["head", "body"], "current": "head"},
            {"path": "groups/a-cap/enabled", "type": "boolean", "current": true, "default": true},
            {"path": "tags", "type": "list", "current": ["a"]}
        ]})
    }

    #[test]
    fn paths_become_nested_sections_with_ranges_units_and_defaults() {
        let params = read(&answer(), "");
        assert_eq!(params.fields, 5);
        let width = &params.schema["properties"]["cranium"]["properties"]["half_width"];
        assert_eq!(width["minimum"], json!(0.05));
        assert_eq!(width["x-ui"]["unit"], json!("m"));
        assert_eq!(width["title"], json!("Half width"));
        assert_eq!(params.schema["properties"]["cranium"]["description"], json!("The cranium."));
        assert_eq!(params.value["cranium"]["half_width"], json!(0.2));
        assert_eq!(params.value["cranium"]["centre"]["0"], json!(0.0), "a list item is a field keyed by its index");
        let first = &params.schema["properties"]["cranium"]["properties"]["centre"]["properties"]["0"];
        assert_eq!(first["title"], json!("Centre 1"), "and named by its list and its place");
        assert_eq!(params.value["groups"]["a-cap"]["dynamics"]["follow"], json!("head"));
        assert_eq!(params.schema["properties"]["tags"]["type"], json!("array"), "a list set whole is shown");
    }

    #[test]
    fn a_query_keeps_matching_fields_and_their_sections() {
        let params = read(&answer(), "WIDTH");
        assert_eq!(params.fields, 1);
        assert!(params.schema["properties"].get("groups").is_none());
        let by_description = read(&answer(), "half the");
        assert_eq!(by_description.fields, 1);
    }
}
