//! The shared mechanics of the body ops: finding the character's body part, reading its parameters, refusing an edit
//! that leaves a field out of range, writing only the leaves an edit changes, and naming a misspelt name with the
//! closest valid ones.

use std::collections::BTreeSet;

use omuzaki_char_api::{BodyParams, CharId, CharacterDef, PartId, paths};
use omuzaki_doc_api::{Change, DocPath};
use omuzaki_face_api::{ParamErrors, Validate};
use omuzaki_ops_api::{OpCtx, OpError};
use serde_json::{Map, Value};

use crate::ops::character::access;

/// The kind id of the body part: the `body` kind builds the bare body the other body ops edit.
pub(crate) const BODY_KIND: &str = "body";

/// The fix for a value outside its field's range.
const RANGE_HINT: &str = "give the field a value inside the range its message gives";

/// The most suggestions a did-you-mean hint lists.
const MAX_SUGGESTIONS: usize = 3;

/// The character's body part as stored: its id, the path of its parameters, and the parameters themselves.
pub(crate) struct Body {
    /// The part's id.
    pub part: PartId,
    /// `characters/<id>/parts/<part>/params`.
    pub params_path: DocPath,
    /// The parameters, with the defaults filled in.
    pub params: BodyParams,
}

/// The body part of `def`, which is the character `character`.
///
/// # Errors
/// `not_found` (field `character`) when the character has no body part, and `conflict` when it has more than one. The
/// stored parameters must match the body format (`invalid`, naming the part's params).
pub(crate) fn load(character: &CharId, def: &CharacterDef) -> Result<Body, OpError> {
    let mut bodies = def.parts.iter().filter(|(_, part)| part.kind.as_str() == BODY_KIND);
    let (part, stored) = match (bodies.next(), bodies.next()) {
        (Some(only), None) => only,
        (None, _) => {
            return Err(access::not_found("character", format!("character `{character}` has no body part"))
                .hint("add a part of kind `body` with part.add, then retry"));
        }
        (Some(_), Some(_)) => {
            return Err(OpError::conflict(format!("character `{character}` has more than one body part"))
                .hint("keep one body part: the body ops edit the only one"));
        }
    };
    let params = serde_json::from_value(stored.params.clone()).map_err(|e| {
        OpError::invalid(
            format!("parts/{part}/params"),
            format!("the body's parameters don't match the body format: {e}"),
        )
        .hint("repair the body's parameters in the character file, then retry")
    })?;
    Ok(Body { part: part.clone(), params_path: paths::part_params(character, part), params })
}

/// Refuses parameters that fail their own checks, naming the first field (`shape/build`) and its range. Every problem
/// is listed, so one refusal shows the whole edit's faults.
///
/// # Errors
/// `invalid`, naming the first field that is out of range.
pub(crate) fn refuse_invalid(after: &BodyParams) -> Result<(), OpError> {
    let Err(ParamErrors(errors)) = after.validate() else {
        return Ok(());
    };
    let Some(first) = errors.first() else {
        return Ok(());
    };
    let list: Vec<String> = errors.iter().map(|e| format!("{}: {}", e.path, e.message)).collect();
    Err(OpError::invalid(first.path.clone(), list.join("; ")).hint(RANGE_HINT))
}

/// The refusal for a name the body does not have: it names the argument `field`, and suggests the closest valid names
/// (`did you mean`) before listing them all.
pub(crate) fn unknown_name(field: &str, what: &str, wanted: &str, known: &[&str]) -> OpError {
    let suggestions = closest(wanted, known);
    let did = match suggestions.as_slice() {
        [] => String::new(),
        [one] => format!("did you mean `{one}`? "),
        many => {
            let quoted: Vec<String> = many.iter().map(|s| format!("`{s}`")).collect();
            format!("did you mean {}? ", quoted.join(" or "))
        }
    };
    OpError::invalid(field, format!("`{wanted}` is not a {what}"))
        .hint(format!("{did}valid choices: {}", known.join(", ")))
}

/// The names closest to `wanted`, best first: names that contain it (or are contained in it), then names within a small
/// edit distance.
fn closest<'a>(wanted: &str, candidates: &[&'a str]) -> Vec<&'a str> {
    let wanted = wanted.to_lowercase();
    let allowed = (wanted.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, &str)> = candidates
        .iter()
        .filter_map(|name| {
            let lower = name.to_lowercase();
            let related = wanted.len() >= 3 && (lower.contains(&wanted) || wanted.contains(&lower));
            let distance = if related { 0 } else { edit_distance(&wanted, &lower) };
            (distance <= allowed).then_some((distance, *name))
        })
        .collect();
    scored.sort_unstable();
    scored.into_iter().take(MAX_SUGGESTIONS).map(|(_, name)| name).collect()
}

/// Levenshtein distance over chars, with two rows of working memory.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != *cb);
            cur.push(substitute.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The changes that turn the stored parameters at `path` into `after`. Only the leaves that differ between the typed
/// `before` and `after` are written, so a default the file leaves out is not written, and undo removes exactly what the
/// edit added.
///
/// # Errors
/// `Internal` if the parameters do not serialise, which would be a bug in `omuzaki-char-api`.
pub(crate) fn changes(
    ctx: &OpCtx<'_>,
    path: &DocPath,
    before: &BodyParams,
    after: &BodyParams,
) -> Result<Vec<Change>, OpError> {
    let before = to_value(before)?;
    let after = to_value(after)?;
    let mut out = Vec::new();
    diff(path, ctx.doc.get(path), &before, &after, &mut out);
    Ok(out)
}

/// The parameters as a JSON value, the form the diff compares.
fn to_value(params: &BodyParams) -> Result<Value, OpError> {
    serde_json::to_value(params).map_err(|e| OpError::internal(format!("body parameters do not serialise: {e}")))
}

/// Appends the changes that take the node at `path` (stored as `node`, if the document has it) from `before` to `after`.
fn diff(path: &DocPath, node: Option<&Value>, before: &Value, after: &Value, out: &mut Vec<Change>) {
    if before == after {
        return;
    }
    match (node, before, after) {
        (None, _, _) => {
            if let Some(created) = delta(before, after) {
                out.push(Change { path: path.clone(), before: None, after: Some(created) });
            }
        }
        (Some(Value::Object(stored)), Value::Object(b), Value::Object(a)) => {
            let keys: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
            for key in keys {
                let (old, new) = (b.get(key).unwrap_or(&Value::Null), a.get(key).unwrap_or(&Value::Null));
                diff(&path.join(key.as_str()), stored.get(key), old, new, out);
            }
        }
        (Some(stored), _, _) => out.push(Change {
            path: path.clone(),
            before: Some(stored.clone()),
            after: (!after.is_null()).then(|| after.clone()),
        }),
    }
}

/// What a node the document lacks must hold so that it reads as `after`: only the values that differ from `before`.
/// `None` when nothing needs writing (or `after` removes the node, which it already lacks).
fn delta(before: &Value, after: &Value) -> Option<Value> {
    if before == after || after.is_null() {
        return None;
    }
    match (before, after) {
        (Value::Object(b), Value::Object(a)) => {
            let fields: Map<String, Value> = a
                .iter()
                .filter_map(|(key, value)| {
                    delta(b.get(key).unwrap_or(&Value::Null), value).map(|changed| (key.clone(), changed))
                })
                .collect();
            (!fields.is_empty()).then_some(Value::Object(fields))
        }
        _ => Some(after.clone()),
    }
}

/// The paths a patch changes, as text, for an op's output.
pub(crate) fn changed_paths(changes: &[Change]) -> Vec<String> {
    changes.iter().map(|c| c.path.to_string()).collect()
}
