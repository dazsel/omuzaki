//! The mechanics the layout ops share: reading the layout's fields, refusing an edit that introduces a problem, and
//! turning new field values into changes at `face/layout`.
//!
//! A layout change never writes a `null` and never touches anything but `face/layout` (or the face table that holds it,
//! when a character has no face yet). Each changed field is its own change, with the value the document holds as its
//! `before`, so the undo is exact even where a file left a default out.

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{Change, DocPath};
use omuzaki_face_api::{FaceLayout, ParamError, ParamErrors, Validate};
use omuzaki_ops_api::{OpCtx, OpError};
use serde_json::{Map, Value, json};

use crate::ops::character::access;

/// The fix for a field whose problem names none of its own.
const RANGE_HINT: &str = "give the field a value inside the range its message gives";

/// The layout as a JSON object with one key per field, which is how a single field is named and set.
///
/// # Errors
/// `Internal` if the layout does not serialise as an object, which would be a bug in `omuzaki-face-api`.
pub(super) fn layout_fields(layout: &FaceLayout) -> Result<Map<String, Value>, OpError> {
    match serde_json::to_value(layout) {
        Ok(Value::Object(fields)) => Ok(fields),
        _ => Err(OpError::internal("a face layout does not serialise as an object")),
    }
}

/// The layout after `value` is written to `field`, or `invalid` naming the field when the layout has no such field.
///
/// # Errors
/// `Invalid` naming `face/layout/<field>` and listing the fields the layout has.
pub(super) fn with_field(current: &FaceLayout, field: &str, value: f64) -> Result<FaceLayout, OpError> {
    if !value.is_finite() {
        return Err(OpError::invalid(format!("face/layout/{field}"), format!("{value} is not a finite number")));
    }
    let mut fields = layout_fields(current)?;
    if !fields.contains_key(field) {
        let known: Vec<&str> = fields.keys().map(String::as_str).collect();
        return Err(OpError::invalid(format!("face/layout/{field}"), format!("the layout has no field '{field}'"))
            .hint(format!("the fields are {}", known.join(", "))));
    }
    fields.insert(field.to_owned(), json!(value));
    serde_json::from_value(Value::Object(fields))
        .map_err(|e| OpError::internal(format!("a layout field did not read back as a layout: {e}")))
}

/// Every problem in `layout`, as the path it names in the face (`face/layout/eye_y`) and its message.
fn problems(layout: &FaceLayout) -> Vec<(String, String)> {
    match layout.validate() {
        Ok(()) => Vec::new(),
        Err(ParamErrors(errors)) => errors.into_iter().map(|e| named("face/layout", &e)).collect(),
    }
}

/// `error` with its path under `prefix`, as `prefix/path`.
fn named(prefix: &str, error: &ParamError) -> (String, String) {
    (format!("{prefix}/{}", error.path), error.message.clone())
}

/// Refuses an edit that makes `after` invalid in a way `current` is not. The problems the layout already has are not
/// blamed on the edit, so a layout damaged elsewhere can still take an unrelated change.
///
/// # Errors
/// `Invalid`, naming the first problem the edit introduces and listing all of them.
pub(crate) fn refuse_new_problems(current: &FaceLayout, after: &FaceLayout) -> Result<(), OpError> {
    let before = problems(current);
    let introduced: Vec<(String, String)> = problems(after).into_iter().filter(|p| !before.contains(p)).collect();
    let Some((first, _)) = introduced.first() else {
        return Ok(());
    };
    let count = introduced.len();
    let noun = if count == 1 { "field is" } else { "fields are" };
    let list = introduced.iter().map(|(path, message)| format!("{path}: {message}")).collect::<Vec<_>>().join("; ");
    Err(OpError::invalid(first.clone(), format!("{count} {noun} invalid: {list}")).hint(RANGE_HINT))
}

/// Refuses ratios that fail their own checks, naming each field as `<prefix>/<field>` with the range it accepts.
///
/// # Errors
/// `Invalid`, naming the first bad ratio and listing every one.
pub(super) fn refuse_bad_ratios(ratios: &impl Validate, prefix: &str) -> Result<(), OpError> {
    let Err(ParamErrors(errors)) = ratios.validate() else {
        return Ok(());
    };
    let named_errors: Vec<(String, String)> = errors.iter().map(|e| named(prefix, e)).collect();
    let Some((first, _)) = named_errors.first() else {
        return Ok(());
    };
    let list = named_errors.iter().map(|(path, message)| format!("{path}: {message}")).collect::<Vec<_>>();
    Err(OpError::invalid(first.clone(), list.join("; "))
        .hint("give each ratio a value inside the range its message gives"))
}

/// The fields that differ between `current` and `target`, with the value `target` gives each one, in field order.
///
/// # Errors
/// `Internal` if a layout does not serialise as an object.
pub(crate) fn changed_fields(current: &FaceLayout, target: &FaceLayout) -> Result<Vec<(String, f64)>, OpError> {
    let before = layout_fields(current)?;
    let after = layout_fields(target)?;
    Ok(after
        .iter()
        .filter_map(|(name, value)| {
            let now = value.as_f64()?;
            (before.get(name).and_then(Value::as_f64) != Some(now)).then(|| (name.clone(), now))
        })
        .collect())
}

/// The changes that set each of `fields` (name and new value) in the character's layout. A character with no face gets
/// the face table with the layout in it; one with a face but no layout gets the layout table; otherwise each field is a
/// change of its own.
///
/// # Errors
/// `Conflict` when the stored `face` or `face/layout` is not an object, so it cannot hold the fields.
pub(crate) fn layout_changes(
    ctx: &OpCtx<'_>,
    character: &CharId,
    fields: &[(String, f64)],
) -> Result<Vec<Change>, OpError> {
    if fields.is_empty() {
        return Ok(Vec::new());
    }
    let face = paths::face(character);
    let layout = face.join("layout");
    let written = fields_object(fields);
    match ctx.doc.get(&face) {
        None => Ok(vec![Change { path: face, before: None, after: Some(json!({ "layout": written })) }]),
        Some(Value::Object(stored)) => match stored.get("layout") {
            None => Ok(vec![Change { path: layout, before: None, after: Some(written) }]),
            Some(Value::Object(_)) => {
                Ok(fields.iter().map(|(name, value)| field_change(ctx, &layout, name, *value)).collect())
            }
            Some(_) => Err(not_an_object(&layout)),
        },
        Some(_) => Err(not_an_object(&face)),
    }
}

/// One field's change at `layout/<name>`, expecting what the document holds there now.
fn field_change(ctx: &OpCtx<'_>, layout: &DocPath, name: &str, value: f64) -> Change {
    access::change(ctx, layout.join(name), Some(json!(value)))
}

/// The JSON object of the given fields.
fn fields_object(fields: &[(String, f64)]) -> Value {
    Value::Object(fields.iter().map(|(name, value)| (name.clone(), json!(value))).collect())
}

/// The error for a face or layout that is not an object.
fn not_an_object(path: &DocPath) -> OpError {
    OpError::conflict(format!("{path} is not an object, so it can't hold the layout"))
        .hint("fix the character file, or recreate its face")
}
