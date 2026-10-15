//! JSON and script values: the bounded conversion between them, and the size checks on what crosses the sandbox.

use omuzaki_doc_api::Value;
use rhai::Dynamic;
use rhai::Map;
use rhai::serde::{from_dynamic, to_dynamic};
use serde_json::Map as JsonMap;

use crate::{LimitKind, Limits};

/// The first collection limit a JSON value breaks, if any. Values converted outside the engine (`params`, op outputs)
/// are checked with this before conversion: Rhai doesn't size values pushed into the scope, and never checks map keys.
/// Iterative, so a deeply nested value can't overflow the stack.
pub(crate) fn oversized(value: &Value, limits: &Limits) -> Option<LimitKind> {
    oversized_in(value, limits, 0)
}

/// [`oversized`] for a value that sits `outer` containers deep already (a `params` field is inside the `params` map).
/// Nesting beyond [`Limits::value_depth`] is [`LimitKind::Depth`], so the recursive conversion into Rhai is bounded.
pub(crate) fn oversized_in(value: &Value, limits: &Limits, outer: usize) -> Option<LimitKind> {
    let mut pending = vec![(value, outer)];
    while let Some((value, depth)) = pending.pop() {
        match value {
            Value::String(text) if text.len() > limits.string_bytes => return Some(LimitKind::StringSize),
            Value::Array(_) | Value::Object(_) if depth >= limits.value_depth => return Some(LimitKind::Depth),
            Value::Array(items) if items.len() > limits.array_len => return Some(LimitKind::ArraySize),
            Value::Array(items) => pending.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(fields) => {
                if let Some(kind) = map_breaks(fields, limits) {
                    return Some(kind);
                }
                pending.extend(fields.values().map(|field| (field, depth + 1)));
            }
            _ => {}
        }
    }
    None
}

/// Why [`to_json`] failed.
pub(crate) enum Convert {
    /// Arrays and maps nest deeper than [`Limits::value_depth`].
    Depth,
    /// A value with no JSON form.
    Other(String),
}

/// One array or map being converted: what is left to visit and what has been converted so far.
struct Frame {
    rest: Box<dyn Iterator<Item = (String, Dynamic)>>,
    key: String,
    done: Done,
}

enum Done {
    Array(Vec<Value>),
    Object(JsonMap<String, Value>),
}

/// A script value as JSON, walked with an explicit stack: rhai's `from_dynamic` recurses once per level, so a script
/// that wraps a value in an array in a loop could overflow the stack from inside the limits on size and operations.
/// Fails with [`Convert::Depth`] beyond `max_depth` levels of arrays and maps. The value is consumed, and on failure
/// whatever is left is taken apart without recursion as well, so dropping a deep value can't overflow either.
pub(crate) fn to_json(root: Dynamic, max_depth: usize) -> Result<Value, Convert> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut next = Some(root);
    let mut finished: Option<Value> = None;
    loop {
        if let Some(item) = next.take() {
            let item = item.flatten();
            if item.is_array() || item.is_map() {
                if stack.len() >= max_depth {
                    abandon(stack, Some(item));
                    return Err(Convert::Depth);
                }
                stack.push(open(item));
            } else {
                match from_dynamic::<Value>(&item) {
                    Ok(value) => finished = Some(value),
                    Err(e) => {
                        abandon(stack, None);
                        return Err(Convert::Other(e.to_string()));
                    }
                }
            }
        }
        if let Some(value) = finished.take() {
            let Some(top) = stack.last_mut() else { return Ok(value) };
            match &mut top.done {
                Done::Array(items) => items.push(value),
                Done::Object(fields) => {
                    fields.insert(std::mem::take(&mut top.key), value);
                }
            }
        }
        let Some(top) = stack.last_mut() else {
            return Err(Convert::Other("the conversion lost its place".to_owned()));
        };
        match top.rest.next() {
            Some((key, item)) => {
                top.key = key;
                next = Some(item);
            }
            None => {
                finished = stack.pop().map(|frame| match frame.done {
                    Done::Array(items) => Value::Array(items),
                    Done::Object(fields) => Value::Object(fields),
                });
            }
        }
    }
}

/// Starts converting an array or map.
fn open(container: Dynamic) -> Frame {
    if container.is_array() {
        let items = container.into_array().unwrap_or_default();
        let done = Done::Array(Vec::with_capacity(items.len()));
        Frame { rest: Box::new(items.into_iter().map(|item| (String::new(), item))), key: String::new(), done }
    } else {
        let fields = container.try_cast::<Map>().unwrap_or_default();
        let done = Done::Object(JsonMap::new());
        Frame { rest: Box::new(fields.into_iter().map(|(k, v)| (k.to_string(), v))), key: String::new(), done }
    }
}

/// Drops what a failed conversion still holds, one level at a time, so no destructor recurses.
fn abandon(stack: Vec<Frame>, extra: Option<Dynamic>) {
    let mut pending: Vec<Dynamic> = extra.into_iter().collect();
    for frame in stack {
        pending.extend(frame.rest.map(|(_, item)| item));
    }
    while let Some(item) = pending.pop() {
        let item = item.flatten();
        if item.is_array() {
            pending.extend(item.into_array().unwrap_or_default());
        } else if item.is_map() {
            pending.extend(item.try_cast::<Map>().unwrap_or_default().into_values());
        }
    }
}

/// The limit a map itself breaks: its size, or a key (which becomes a Rhai string) over the string limit.
pub(crate) fn map_breaks(fields: &JsonMap<String, Value>, limits: &Limits) -> Option<LimitKind> {
    if fields.len() > limits.map_len {
        return Some(LimitKind::MapSize);
    }
    fields.keys().any(|key| key.len() > limits.string_bytes).then_some(LimitKind::StringSize)
}

pub(crate) fn size_kind(typ: &str) -> LimitKind {
    let typ = typ.to_ascii_lowercase();
    if typ.contains("string") {
        LimitKind::StringSize
    } else if typ.contains("array") {
        LimitKind::ArraySize
    } else {
        LimitKind::MapSize
    }
}

/// A JSON value as a script value: maps become Rhai maps (sorted by key, so iteration is deterministic), integers
/// stay `i64`, and `null` is `()`.
///
/// # Errors
/// A number with no script form, which `serde_json` can't produce from valid JSON.
pub(crate) fn from_json(value: &Value) -> Result<Dynamic, String> {
    to_dynamic(value).map_err(|e| e.to_string())
}
