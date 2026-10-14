//! The mechanics the expression ops share beyond the face helpers in `ops::shared::edit`: the expression library inside a
//! stored face, the entries the ops look up, and the working copy each op edits. As in every face op, an edit writes a
//! copy of the face and diffs it against the stored one, so the patch names the smallest set of changes and undo is exact.

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::Patch;
use omuzaki_face_api::{Expression, ExpressionId};
use omuzaki_ops_api::OpError;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::ops::shared::edit::diff;

/// The expression library inside a stored face: `None` when the face has no `expressions` yet.
///
/// # Errors
/// `Conflict` when `expressions` is present but is not an object.
pub(super) fn library(face: Option<&Value>) -> Result<Option<&Map<String, Value>>, OpError> {
    let Some(face) = face else {
        return Ok(None);
    };
    match face.get("expressions") {
        None => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map)),
        Some(_) => Err(OpError::conflict("face/expressions is not an object, so it can't hold expressions")
            .hint("fix the character file")),
    }
}

/// The stored entry called `id`, if the library holds one.
pub(super) fn entry<'a>(library: Option<&'a Map<String, Value>>, id: &ExpressionId) -> Option<&'a Value> {
    library.and_then(|map| map.get(id.as_str()))
}

/// A stored entry read as an expression. An entry the file doesn't read is refused, and the error names it.
///
/// # Errors
/// `Invalid`, naming `face/expressions/<id>`.
pub(super) fn read_entry(value: &Value, id: &ExpressionId) -> Result<Expression, OpError> {
    Expression::deserialize(value).map_err(|e| {
        OpError::invalid(
            format!("face/expressions/{id}"),
            format!("the expression '{id}' in the document doesn't read: {e}"),
        )
        .hint("fix the character file: an expression needs a kind and deltas, and may have a curve")
    })
}

/// The refusal for a name the character doesn't have.
pub(super) fn missing(id: &ExpressionId) -> OpError {
    OpError::not_found(format!("there is no expression '{id}' on this character"))
        .hint("`face.expression.set` creates one; the names are the keys of face/expressions")
}

/// The refusal for a name that is already taken, when an op would overwrite it. `field` is the argument that names it.
pub(super) fn taken(id: &ExpressionId, field: &str) -> OpError {
    OpError::invalid(field, format!("there is already an expression '{id}'"))
        .hint("delete it first, or pick another name")
}

/// The stored face with its expression library edited by `edit`: a copy of `stored`, with `expressions` created when the
/// face has none. The caller has already refused a library that isn't an object (see [`library`]).
///
/// # Errors
/// `Internal` if the stored face or its library is not an object, which the caller rules out.
pub(super) fn edited_face(
    stored: Option<&Value>,
    edit: impl FnOnce(&mut Map<String, Value>),
) -> Result<Value, OpError> {
    let mut face = stored.cloned().unwrap_or_else(|| Value::Object(Map::new()));
    let entries = face
        .as_object_mut()
        .ok_or_else(|| OpError::internal("a face is an object"))?
        .entry("expressions")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| OpError::internal("the expression library is an object, checked before the edit"))?;
    edit(entries);
    Ok(face)
}

/// The patch that turns the stored face into `working`, as the face ops write it: the smallest set of changes that
/// reaches it. A character with no face gets the face in one change; a face with no library gets the library in one.
pub(super) fn patch_to(character: &CharId, stored: Option<&Value>, working: &Value) -> Patch {
    let mut patch = Patch::new();
    diff(&paths::face(character), stored, Some(working), &mut patch);
    patch
}

/// The patch that replaces one stored expression with `after`, as a single change at its path. A diff would name the
/// replaced entry's deltas one by one (eight changes for the starter `happy`), so an expression is replaced whole, as the
/// plan's output has always listed it.
pub(super) fn replace_entry(character: &CharId, id: &ExpressionId, before: Value, after: Value) -> Patch {
    Patch::new().with(paths::face(character).join("expressions").join(id.as_str()), Some(before), Some(after))
}
