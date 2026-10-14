//! The mechanics the face ops share: reading a character's face, writing a value into a working copy of it, and turning
//! the copy into the smallest patch. Validation lives in `check`.
//!
//! An op never writes the face in place. It copies the stored JSON, writes its change into the copy, has the copy checked
//! against what the stored face already has, and only then diffs the copy against the stored value. The patch therefore
//! names the leaves that changed (or the one new object that holds them), and a refusal changes nothing.

use std::collections::BTreeSet;

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{Change, DocPath, Patch};
use omuzaki_face_api::FaceProfile;
use omuzaki_ops_api::{OpCtx, OpError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::path::kind_of;

/// The character's face as the document holds it, or `None` when the character has no face yet.
///
/// # Errors
/// `NotFound` if there is no such character; `Conflict` if its `face` is not an object.
pub(crate) fn stored_face(ctx: &OpCtx<'_>, character: &CharId) -> Result<Option<Value>, OpError> {
    if ctx.doc.get(&paths::character(character)).is_none() {
        return Err(
            OpError::not_found(format!("there is no character '{character}'")).hint("`character.create` makes one")
        );
    }
    match ctx.doc.get(&paths::face(character)) {
        None => Ok(None),
        Some(face @ Value::Object(_)) => Ok(Some(face.clone())),
        Some(_) => {
            Err(OpError::conflict(format!("{} is not an object, so it can't hold a face", paths::face(character)))
                .hint("fix the character file, or recreate its face"))
        }
    }
}

/// How many marks the stored face holds; none when it has no list yet.
pub(crate) fn marks_len(stored: Option<&Value>) -> usize {
    stored.and_then(|face| face.get("marks")).and_then(Value::as_array).map_or(0, Vec::len)
}

/// The face the renderer sees: the stored value with the defaults for every key it leaves out.
///
/// # Errors
/// `Invalid` naming `face` when the stored value doesn't read as a face.
pub(crate) fn effective(stored: Option<&Value>) -> Result<FaceProfile, OpError> {
    let Some(value) = stored else {
        return Ok(FaceProfile::default());
    };
    FaceProfile::deserialize(value).map_err(|e| {
        OpError::invalid("face", format!("the face in the document doesn't read: {e}"))
            .hint("fix the character file; `face.describe_style` can't read a face that doesn't parse")
    })
}

/// The first `null` in `value`, as the path it sits at. A null would be written into the document, which never holds one.
pub(crate) fn first_null(value: &Value, at: &str) -> Option<String> {
    match value {
        Value::Null => Some(at.to_owned()),
        Value::Object(map) => map.iter().find_map(|(key, child)| first_null(child, &format!("{at}/{key}"))),
        Value::Array(items) => items.iter().enumerate().find_map(|(i, child)| first_null(child, &format!("{at}/{i}"))),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

/// Writes `value` at `keys` under `node`, the face or a container below it, which the document path `at` names.
/// `chain[d]` is the default that `keys[d]` holds, so a missing fixed-length list (such as `radius`) is created whole,
/// while a missing object is created holding only the keys on the way; the file's defaults fill in the rest when it is
/// read. A value that is in the way (a number where a section should be) is refused rather than overwritten.
///
/// # Errors
/// `Invalid`, naming the value in the way, when the stored face holds something other than a section or a list there.
pub(crate) fn put(node: &mut Value, at: &str, keys: &[String], chain: &[&Value], value: Value) -> Result<(), OpError> {
    let (key, rest) = keys.split_first().ok_or_else(|| OpError::internal("a face edit needs a key"))?;
    let default = chain.first().copied().ok_or_else(|| OpError::internal("a face edit needs a default"))?;
    let below = chain.get(1..).unwrap_or(&[]);
    let here = format!("{at}/{key}");
    match node {
        Value::Object(map) if rest.is_empty() => {
            match map.get_mut(key) {
                Some(slot) => assign(slot, value),
                None => {
                    map.insert(key.clone(), value);
                }
            }
            Ok(())
        }
        Value::Object(map) => {
            let child = map.entry(key.clone()).or_insert_with(|| empty_container(default));
            if !matches!(child, Value::Object(_) | Value::Array(_)) {
                return Err(OpError::invalid(
                    &here,
                    format!("{here} holds {}, not a section, so it can't be edited inside", kind_of(child)),
                )
                .hint(format!("set {here} to a whole section first")));
            }
            put(child, &here, rest, below, value)
        }
        Value::Array(items) => {
            let len = items.len();
            let slot = key.parse::<usize>().ok().and_then(|i| items.get_mut(i));
            let Some(slot) = slot else {
                return Err(OpError::invalid(
                    &here,
                    format!("{at} is a list of {len}, so '{key}' is not a position in it"),
                )
                .hint(format!("set {at} as a whole list first")));
            };
            if rest.is_empty() {
                assign(slot, value);
                Ok(())
            } else {
                put(slot, &here, rest, below, value)
            }
        }
        _ => Err(OpError::invalid(at, format!("{at} holds {}, so it has no '{key}'", kind_of(node)))),
    }
}

/// Sets `slot` to `value`. Two objects merge key by key, so an object value names the fields it changes and leaves the
/// rest alone; anything else replaces the slot, and the check after the write decides whether that is allowed.
fn assign(slot: &mut Value, value: Value) {
    match (slot, value) {
        (Value::Object(target), Value::Object(given)) => {
            for (key, child) in given {
                match target.get_mut(&key) {
                    Some(existing) => assign(existing, child),
                    None => {
                        target.insert(key, child);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}

/// The empty container a missing key is created as: a fixed-length list keeps its default entries, anything else starts
/// as an empty object.
fn empty_container(default: &Value) -> Value {
    if default.is_array() { default.clone() } else { Value::Object(Map::new()) }
}

/// The changes that turn `before` into `after` at `path`, down to the leaves. Objects are compared key by key and lists
/// of equal length index by index; anything else, including a key or a list that is new, is one change carrying the
/// whole value. Nothing is ever written as `null`.
pub(crate) fn diff(path: &DocPath, before: Option<&Value>, after: Option<&Value>, into: &mut Patch) {
    if before == after {
        return;
    }
    match (before, after) {
        (Some(Value::Object(old)), Some(Value::Object(new))) => {
            let keys: BTreeSet<&str> = old.keys().chain(new.keys()).map(String::as_str).collect();
            for key in keys {
                diff(&path.join(key), old.get(key), new.get(key), into);
            }
        }
        (Some(Value::Array(old)), Some(Value::Array(new))) if old.len() == new.len() => {
            for (i, (was, now)) in old.iter().zip(new).enumerate() {
                diff(&path.join(i.to_string()), Some(was), Some(now), into);
            }
        }
        _ => into.changes.push(Change { path: path.clone(), before: before.cloned(), after: after.cloned() }),
    }
}

/// The document paths an edit changes, and nothing else: the plan's output.
pub(crate) fn changed(patch: &Patch) -> Vec<String> {
    patch.changes.iter().map(|c| c.path.to_string()).collect()
}

/// Serialises a typed value for the document or for output.
///
/// # Errors
/// `Internal` if serialisation fails, which would be a bug in the type.
pub(crate) fn to_json<T: Serialize + ?Sized>(value: &T) -> Result<Value, OpError> {
    serde_json::to_value(value).map_err(|e| OpError::internal(format!("serialising a face value failed: {e}")))
}
