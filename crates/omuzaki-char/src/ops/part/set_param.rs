//! `part.set_param`: set one parameter of a part by path, checked by the part's kind, as the smallest patch that makes
//! the change.
//!
//! The edit is made on a copy of the part's normalised parameters, the kind's own `check` has the last word on the
//! result, and the patch is the difference between the normalised parameters before and after. So the value written is
//! always what `check` returns (defaults filled in, numbers in their canonical form), a value equal to the current one
//! changes nothing, and a refusal changes nothing. The paths and types come from the same model `part.describe` shows
//! ([`Params`]), so an agent that copies a path from there cannot be told it does not exist.

use std::collections::BTreeSet;

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_doc_api::{Change, DocPath, Value};
use omuzaki_face_api::{ParamError, ParamErrors};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::describe::{Param, Params, Ty, at, kind_name, kind_of, nearest, set_at};
use crate::ops::character::access;

/// Sets one parameter of a part: `path` names it as `part.describe` lists it (`cranium/half_width`, or
/// `groups/a-cap/dynamics/simulate` for a hair group), and `value` is the new value in the type it holds: a number,
/// `true` / `false`, text, or one of the allowed names. The value is checked by the part's kind, and a refusal names the
/// parameter and what it accepts (its range, or the allowed names) and changes nothing. Setting a value the parameter
/// already has changes nothing. An object value sets the fields it names and leaves the others, so a whole section
/// can be edited at once, and a new map entry (a hair group) is added by setting the map to an object holding it; a
/// list is replaced whole. `null` removes an optional parameter (the chin of a head), and is refused for any other.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "path": "shading/mid", "value": "#6b4a3a"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PartSetParam {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part to change.
    pub part: PartId,
    /// The parameter, `/`-separated from the part's parameters. `part.describe` lists every path, and a wrong one is
    /// refused with the nearest valid one.
    pub path: String,
    /// The new value, of the type the parameter holds. Numbers must be within the range `part.describe` gives.
    pub value: Value,
}

impl Op for PartSetParam {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.set_param",
            summary: "Set one parameter of a part by path; the kind validates it, and a refusal names the parameter \
                      and what it accepts.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let part = access::part(&def, &self.character, &self.part, "part")?;
        let kind = kind_of(&self.part, part)?;
        let params = Params::new(kind, &part.params)?;
        let param = params.resolve(&self.path)?;
        let value = shaped(&params, param, &self.value)?;

        // A part whose stored parameters already fail the check is edited as stored, so one set can repair it.
        let mut working = params.current.as_ref().map_or_else(|_| params.stored.clone(), Clone::clone);
        set_at(&mut working, &self.path, value)?;
        let after = (kind.check)(&working).map_err(|errors| refusal(&params, param, &self.value, errors))?;

        let mut changes = Vec::new();
        diff(ctx, &paths::part_params(&self.character, &self.part), Some(params.shown()), Some(&after), &mut changes);
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "path": self.path,
            "before": at(params.shown(), &self.path),
            "after": at(&after, &self.path),
            "changed": !changes.is_empty(),
            "changes": changes.iter().map(|c| c.path.to_string()).collect::<Vec<_>>(),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartSetParam);

/// What to write for `value` at `param`: `value` itself once it has the type the parameter holds, or `None` to remove
/// an optional parameter. A mismatch the kind's `check` would only report as "params: invalid type" (serde does not
/// name the field) is caught here, with the parameter's path and what it takes.
///
/// # Errors
/// `invalid`, naming the parameter, for a value of the wrong type, a name that is not allowed, a fraction for a whole
/// number, or `null` for a parameter that is not optional.
fn shaped(params: &Params, param: &Param, value: &Value) -> Result<Option<Value>, OpError> {
    let fits = match (&param.ty, value) {
        (_, Value::Null) => {
            return if param.optional { Ok(None) } else { Err(wrong(params, param, value)) };
        }
        (Ty::Number, Value::Number(_))
        | (Ty::Boolean, Value::Bool(_))
        | (Ty::Text, Value::String(_))
        | (Ty::List, Value::Array(_))
        // A choice that is also an object in other forms (hair `rng`): `check` decides which form it is.
        | (Ty::Map | Ty::Section | Ty::Choice(_), Value::Object(_))
        | (Ty::Any, _) => true,
        (Ty::Integer, Value::Number(n)) => n.is_i64() || n.is_u64(),
        (Ty::Choice(names), Value::String(name)) => names.contains(name),
        _ => false,
    };
    if fits { Ok(Some(value.clone())) } else { Err(wrong(params, param, value)) }
}

/// The refusal for `value` not fitting `param`.
fn wrong(params: &Params, param: &Param, value: &Value) -> OpError {
    let hint = match (&param.ty, value) {
        (Ty::Choice(names), Value::String(name)) => nearest(name, names.iter().map(String::as_str))
            .map_or_else(String::new, |n| format!("did you mean `{n}`? ")),
        (Ty::Integer, Value::Number(_)) => "write it as a whole number, without a decimal point; ".to_owned(),
        (_, Value::Null) => "null removes an optional parameter, and this one always has a value; ".to_owned(),
        _ => String::new(),
    };
    OpError::invalid(
        &param.path,
        format!("`{}` takes {}, but {} was given", param.path, params.takes(param), given(value)),
    )
    .hint(format!("{hint}part.describe shows this parameter's type, range and default"))
}

/// A value as a sentence names it: scalars as written (text cut short), containers by what they are.
fn given(value: &Value) -> String {
    match value {
        Value::String(text) if text.chars().count() > 40 => {
            format!("\"{}...\"", text.chars().take(40).collect::<String>())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.to_string(),
        other => kind_name(other).to_owned(),
    }
}

/// The refusal for an edit the kind's check rejected: every error it found, the first one naming the field, and what
/// that parameter accepts. A decode error (serde names no field) is blamed on the parameter that was set, the only
/// thing the edit changed; for a number out of the width of its type (`300` for a `u8`) serde's wording is replaced
/// by what the parameter takes. Errors the part already had come last, so the ones the edit caused read first.
fn refusal(params: &Params, param: &Param, value: &Value, errors: ParamErrors) -> OpError {
    let known: &[ParamError] = params.current.as_ref().err().map_or(&[], |e| e.0.as_slice());
    let blame = |e: ParamError| {
        let scalar = !matches!(value, Value::Object(_) | Value::Array(_));
        let message =
            if scalar { format!("takes {}, but {} was given", params.takes(param), given(value)) } else { e.message };
        ParamError { path: param.path.clone(), message }
    };
    let mut errors: Vec<ParamError> =
        errors.0.into_iter().map(|e| if e.path == "params" { blame(e) } else { e }).collect();
    errors.sort_by_key(|e| known.contains(e));
    let field = errors.first().map_or(param.path.as_str(), |e| e.path.as_str()).to_owned();
    let message = errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    let hint = match params.entry(&field) {
        Some(param) => {
            format!("`{field}` takes {}; part.describe lists every parameter with its range", params.takes(param))
        }
        None => "part.describe lists every parameter with its type, range and default".to_owned(),
    };
    OpError::invalid(field, message).hint(hint)
}

/// Appends the changes that turn `before` into `after` at `path`, down to the leaves that differ. Only nodes the
/// document holds are descended into: a section the file leaves out (it has defaults) is created whole, with the
/// values `check` returned, because a change needs its parent to exist and the section must read back as it was
/// validated. `before` is read from the document, so undo restores exactly what the file said.
fn diff(ctx: &OpCtx<'_>, path: &DocPath, before: Option<&Value>, after: Option<&Value>, changes: &mut Vec<Change>) {
    if before == after {
        return;
    }
    let stored = ctx.doc.get(path);
    match (before, after) {
        (Some(Value::Object(old)), Some(Value::Object(new))) if stored.is_some_and(Value::is_object) => {
            let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
            for key in keys {
                diff(ctx, &path.join(key.as_str()), old.get(key), new.get(key), changes);
            }
        }
        (Some(Value::Array(old)), Some(Value::Array(new)))
            if old.len() == new.len() && stored.and_then(Value::as_array).is_some_and(|s| s.len() == old.len()) =>
        {
            for (index, (was, now)) in old.iter().zip(new).enumerate() {
                diff(ctx, &path.join(index.to_string()), Some(was), Some(now), changes);
            }
        }
        _ => changes.push(access::change(ctx, path.clone(), after.cloned())),
    }
}
