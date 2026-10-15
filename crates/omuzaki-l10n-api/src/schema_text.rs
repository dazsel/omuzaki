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
///
/// A branch is a variant (a key of its own) only where the schema says so. A constant or a one-value `enum` is always a
/// variant. An object is an internally tagged variant, keyed by its tag, or an externally tagged one, keyed by its one
/// required property, only as an entry of a `oneOf` that has no `null` sibling. Every other branch, including every
/// `anyOf` entry, is transparent. So `Option<T>`, which schemars writes as an `anyOf` with a `null`, keeps `T`'s
/// fields under the field's own key, even when `T` is a struct with a single field.
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
        self.children(node, None);
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

    /// Walks a schema that adds no key to the path and is not visited itself: map values, `allOf`, and the branches
    /// the rules call transparent.
    fn transparent(&mut self, schema: &Value) {
        if let Some(node) = schema.as_object() {
            self.children(node, None);
        }
    }

    /// Walks each schema of an `items` or `prefixItems` keyword, transparently: one schema, or a list of them.
    fn transparent_items(&mut self, items: Option<&Value>) {
        match items {
            Some(Value::Array(list)) => {
                for item in list {
                    self.transparent(item);
                }
            }
            Some(item) => self.transparent(item),
            None => {}
        }
    }

    /// Walks what `node` contains, without visiting `node` itself. `skip` names a property that is not walked: the
    /// tag of an internally tagged variant, which is the variant's key rather than one of its fields.
    fn children(&mut self, node: &Map<String, Value>, skip: Option<&str>) {
        if let Some(Value::Object(properties)) = node.get("properties") {
            for (key, schema) in properties {
                if Some(key.as_str()) != skip {
                    self.property(key, schema);
                }
            }
        }
        // A `null` branch makes the node an `Option`: its other branch is the value itself, never a variant.
        let nullable = ["oneOf", "anyOf"].iter().any(|keyword| branches(node, keyword).iter().any(is_null));
        for (keyword, variants_allowed) in [("oneOf", !nullable), ("anyOf", false)] {
            for branch in branches(node, keyword) {
                self.branch(branch, variants_allowed);
            }
        }
        // A plain `enum` list only when there are no branches: with branches, its values are the branches' own.
        if let (false, Some(Value::Array(values))) = (has_branches(node), node.get("enum")) {
            for value in values {
                self.path.push(value_key(value));
                self.emit(&Map::new());
                self.path.pop();
            }
        }
        self.transparent_items(node.get("items"));
        self.transparent_items(node.get("prefixItems"));
        if let Some(additional) = node.get("additionalProperties") {
            self.transparent(additional);
        }
        if let Some(Value::Array(all)) = node.get("allOf") {
            for schema in all {
                self.transparent(schema);
            }
        }
    }

    /// Handles one branch of a `oneOf` or `anyOf` by the rules of design 3.1: a null is skipped; a constant or a
    /// one-value `enum` is a variant; an object is a tagged variant only in a `oneOf` that `variants_allowed` (see the
    /// module docs); anything else is walked as if its keys were the parent's.
    fn branch(&mut self, branch: &Value, variants_allowed: bool) {
        let Some(node) = branch.as_object() else {
            return;
        };
        if is_null(branch) {
            return;
        }
        if let Some(value) = single_value(node) {
            self.path.push(value_key(value));
            self.emit(node);
            self.path.pop();
            return;
        }
        if variants_allowed {
            if let Some((tag, value)) = internal_tag(node) {
                self.path.push(value);
                self.emit(node);
                // Every child of the variant is walked, including `additionalProperties`, `allOf` and nested unions;
                // only the tag is left out, because it is the variant's key.
                self.children(node, Some(tag.as_str()));
                self.path.pop();
                return;
            }
            if let Some((name, schema)) = external_variant(node) {
                self.path.push(name.to_owned());
                self.emit(node);
                // The property's schema is the variant's content: walked under the variant's key, not visited again.
                self.transparent(schema);
                self.path.pop();
                return;
            }
        }
        self.transparent(branch);
    }
}

/// The branches of a `oneOf` or `anyOf` keyword; none when the keyword is absent or not a list.
fn branches<'a>(node: &'a Map<String, Value>, keyword: &str) -> &'a [Value] {
    match node.get(keyword) {
        Some(Value::Array(list)) => list,
        _ => &[],
    }
}

/// Whether a node has a `oneOf` or `anyOf` list.
fn has_branches(node: &Map<String, Value>) -> bool {
    node.contains_key("oneOf") || node.contains_key("anyOf")
}

/// Whether a branch is the `null` of an `Option`.
fn is_null(branch: &Value) -> bool {
    matches!(branch.get("type"), Some(Value::String(kind)) if kind == "null")
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
