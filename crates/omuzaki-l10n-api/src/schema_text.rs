//! The argument-path walk and the title rule of design `docs/design/l10n-ops.md` §3.1.
//!
//! Every argument id is computed through this module: extraction, the checks, the surfaces and the CLI all walk a
//! schema the same way, so one argument cannot get two ids. The walk follows only the keys the design names. `$ref`
//! and `$defs` are not followed, because op schemas are fully inlined (`omuzaki-ops-api`'s `schema_of`).

use schemars::Schema;
use serde_json::{Map, Value};

/// Visits every node of an argument schema that owns a translatable title or description, with its key path.
///
/// The root is visited first with `[]`, then its properties, enum variants and tag values, depth first, in the
/// schema's key order. A boolean schema has no text and is not visited.
pub fn walk(schema: &Schema, visit: impl FnMut(&[&str], &Map<String, Value>)) {
    let Some(root) = schema.as_object() else {
        return;
    };
    let mut walker = Walker { path: Vec::new(), visit };
    walker.node(root);
}

/// The node's English title: its `title` if it has a non-empty one, else derived from `key`.
///
/// The derived title splits the key on `_` and `-`, joins the words with spaces and upper-cases the first letter:
/// `max_size` gives "Max size", `twin_tails` gives "Twin tails", `Fast` gives "Fast".
pub fn title(node: &Map<String, Value>, key: &str) -> String {
    match node.get("title").and_then(Value::as_str) {
        Some(title) if !title.trim().is_empty() => title.to_owned(),
        _ => humanize(key),
    }
}

/// The node's `description`, trimmed; `None` when absent or blank.
pub fn description(node: &Map<String, Value>) -> Option<&str> {
    node.get("description").and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

/// Derives a title from a key: the words of `max_size` or `max-size` in sentence case.
fn humanize(key: &str) -> String {
    let words: Vec<&str> = key.split(['_', '-']).filter(|word| !word.is_empty()).collect();
    let joined = words.join(" ");
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The walk's state: the key path to the node being visited, and the visitor.
struct Walker<F> {
    path: Vec<String>,
    visit: F,
}

impl<F: FnMut(&[&str], &Map<String, Value>)> Walker<F> {
    /// Visits `node` at the current path, then walks what it contains.
    fn node(&mut self, node: &Map<String, Value>) {
        self.emit(node);
        self.children(node);
    }

    /// Calls the visitor with `node` at the current path.
    fn emit(&mut self, node: &Map<String, Value>) {
        let keys: Vec<&str> = self.path.iter().map(String::as_str).collect();
        (self.visit)(&keys, node);
    }

    /// Visits a property's schema under `key`, then walks it. A boolean schema is not visited.
    fn property(&mut self, key: &str, schema: &Value) {
        let Some(node) = schema.as_object() else {
            return;
        };
        self.path.push(key.to_owned());
        self.node(node);
        self.path.pop();
    }

    /// Walks a schema that adds no key to the path and is not visited itself: items, map values, `allOf`, and the
    /// branches the rules call transparent.
    fn transparent(&mut self, schema: &Value) {
        if let Some(node) = schema.as_object() {
            self.children(node);
        }
    }

    /// Walks what `node` contains, without visiting `node` itself.
    fn children(&mut self, node: &Map<String, Value>) {
        if let Some(Value::Object(properties)) = node.get("properties") {
            for (key, schema) in properties {
                self.property(key, schema);
            }
        }
        let has_branches = node.contains_key("oneOf") || node.contains_key("anyOf");
        for keyword in ["oneOf", "anyOf"] {
            if let Some(Value::Array(branches)) = node.get(keyword) {
                for branch in branches {
                    self.branch(branch);
                }
            }
        }
        // A plain `enum` list only when there are no branches: with branches, its values are the branches' own.
        if let (false, Some(Value::Array(values))) = (has_branches, node.get("enum")) {
            for value in values {
                self.path.push(value_key(value));
                self.emit(&Map::new());
                self.path.pop();
            }
        }
        match node.get("items") {
            Some(Value::Array(items)) => {
                for item in items {
                    self.transparent(item);
                }
            }
            Some(item) => self.transparent(item),
            None => {}
        }
        if let Some(additional) = node.get("additionalProperties") {
            self.transparent(additional);
        }
        if let Some(Value::Array(all)) = node.get("allOf") {
            for schema in all {
                self.transparent(schema);
            }
        }
    }

    /// Handles one `oneOf` or `anyOf` branch by the rules of design 3.1: a null is skipped; a constant or a one-value
    /// `enum` is a variant; an internally or externally tagged object is a variant keyed by its tag; anything else is
    /// walked as if its keys were the parent's.
    fn branch(&mut self, branch: &Value) {
        let Some(node) = branch.as_object() else {
            return;
        };
        if matches!(node.get("type"), Some(Value::String(kind)) if kind == "null") {
            return;
        }
        if let Some(value) = single_value(node) {
            self.path.push(value_key(value));
            self.emit(node);
            self.path.pop();
        } else if let Some((tag, value)) = internal_tag(node) {
            self.path.push(value);
            self.emit(node);
            if let Some(Value::Object(properties)) = node.get("properties") {
                for (key, schema) in properties.iter().filter(|(key, _)| **key != tag) {
                    self.property(key, schema);
                }
            }
            self.path.pop();
        } else if let Some((name, schema)) = external_variant(node) {
            self.path.push(name.to_owned());
            self.emit(node);
            // The property's schema is the variant's content: walked under the variant's key, not visited again.
            self.transparent(schema);
            self.path.pop();
        } else {
            self.transparent(branch);
        }
    }
}

/// The one value a branch stands for: its `const`, or the sole entry of its `enum`.
fn single_value(node: &Map<String, Value>) -> Option<&Value> {
    if let Some(constant) = node.get("const") {
        return Some(constant);
    }
    match node.get("enum") {
        Some(Value::Array(values)) if values.len() == 1 => values.first(),
        _ => None,
    }
}

/// An internally tagged variant's tag: the first property whose schema has a `const`, with its name and value.
fn internal_tag(node: &Map<String, Value>) -> Option<(String, String)> {
    let Value::Object(properties) = node.get("properties")? else {
        return None;
    };
    properties.iter().find_map(|(key, schema)| {
        let constant = schema.as_object()?.get("const")?;
        Some((key.clone(), value_key(constant)))
    })
}

/// An externally tagged variant: an object whose only property is required, with that property's name and schema.
fn external_variant(node: &Map<String, Value>) -> Option<(&str, &Value)> {
    let properties = node.get("properties")?.as_object()?;
    let mut entries = properties.iter();
    let (name, schema) = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    let required = node.get("required")?.as_array()?;
    required.iter().any(|entry| entry.as_str() == Some(name.as_str())).then_some((name.as_str(), schema))
}

/// The key a value contributes to a path: a string as it is, anything else as its JSON text.
fn value_key(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
