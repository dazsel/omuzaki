//! Finding a hair part and writing one branch of its parameters back (`shading`, `physics`, a group), shared by the hair
//! ops that edit more than one group's generator.
//!
//! An op reads the whole part as its kind normalises it, edits the branch it owns, and writes that branch back as one
//! change at the deepest node the document already holds, so undo restores exactly what the file said and a refusal
//! changes nothing.

use omuzaki_char_api::{CharId, CharacterDef, HairParams, PartCategory, PartId, PartKindRegistration, paths};
use omuzaki_doc_api::{Change, DocPath, Value};
use omuzaki_ops_api::{OpCtx, OpError};

use crate::ops::character::access as character;
use crate::ops::character::access::list;
use crate::ops::part::describe::{at, kind_of};

/// A hair part as stored.
pub(crate) struct HairPart {
    /// Where the part's parameters are.
    params_path: DocPath,
    /// The kind's `check`, to validate what is written.
    kind: &'static PartKindRegistration,
    /// The part's parameters as the kind normalises them.
    pub(crate) hair: HairParams,
    /// The character, as stored.
    pub(crate) def: CharacterDef,
}

/// The hair part `part` of `character`.
///
/// # Errors
/// `not_found` on `character` or `part`; `invalid` on `part` for a part that is not hair or whose stored parameters
/// its kind rejects.
pub(crate) fn load(ctx: &OpCtx<'_>, id: &CharId, part: &PartId) -> Result<HairPart, OpError> {
    let def = character::load(ctx, id)?;
    let stored = character::part(&def, id, part, "part")?;
    let kind = kind_of(part, stored)?;
    if (kind.meta)().category != PartCategory::Hair {
        return Err(OpError::invalid("part", format!("part `{part}` is a `{}` part, not hair", stored.kind))
            .hint(format!("hair parts of `{id}`: {}", list(hair_parts(&def)))));
    }
    let normalised = (kind.check)(&stored.params).map_err(|e| {
        OpError::invalid("part", format!("the hair `{part}` is stored with parameters its kind rejects: {e}"))
            .hint("repair the part with part.set_param, then retry")
    })?;
    let hair: HairParams = serde_json::from_value(normalised)
        .map_err(|e| OpError::invalid("part", format!("the hair `{part}` doesn't hold hair parameters: {e}")))?;
    Ok(HairPart { params_path: paths::part_params(id, part), kind, hair, def })
}

impl HairPart {
    /// This part as if it held `hair`: for an op that writes two branches, the second from the state the first left.
    pub(crate) fn holding(&self, hair: HairParams) -> Self {
        Self { params_path: self.params_path.clone(), kind: self.kind, hair, def: self.def.clone() }
    }
}

/// The ids of the character's hair parts, for hints.
pub(crate) fn hair_parts(def: &CharacterDef) -> Vec<String> {
    def.parts
        .iter()
        .filter(|(_, p)| {
            omuzaki_char_api::find_kind(p.kind.as_str()).is_some_and(|k| (k.meta)().category == PartCategory::Hair)
        })
        .map(|(id, _)| id.to_string())
        .collect()
}

/// The changes that store `after`'s branch `steps` (`["shading"]`, `["physics"]`): one change, none when the part
/// already holds it. `after` is checked by the part's kind first; the value written is what the kind normalises.
///
/// # Errors
/// `invalid` on `field` for parameters the part's kind rejects; `internal` if they can't be serialised.
pub(crate) fn changes(
    ctx: &OpCtx<'_>,
    target: &HairPart,
    after: &HairParams,
    steps: &[&str],
    field: &str,
) -> Result<Vec<Change>, OpError> {
    if *after == target.hair {
        return Ok(Vec::new());
    }
    let value = serde_json::to_value(after).map_err(|e| OpError::internal(format!("serialising hair failed: {e}")))?;
    let normalised = (target.kind.check)(&value)
        .map_err(|e| {
            OpError::invalid(field, format!("the hair part refuses the change: {e}"))
                .hint("nothing was changed; give a value the field takes (part.describe lists the fields and their ranges)")
        })?;
    let held =
        (0..=steps.len()).rev().find(|depth| ctx.doc.get(&below(&target.params_path, &steps[..*depth])).is_some());
    let depth = held.unwrap_or(0);
    let path = below(&target.params_path, &steps[..depth]);
    let written = if depth == 0 { Some(normalised) } else { at(&normalised, &steps[..depth].join("/")).cloned() };
    let change = character::change(ctx, path, written);
    Ok(if change.before == change.after { Vec::new() } else { vec![change] })
}

/// `base` with `steps` appended.
fn below(base: &DocPath, steps: &[&str]) -> DocPath {
    steps.iter().fold(base.clone(), |path, step| path.join(*step))
}

/// `value` with `field` of the object `object` set to `to`, read back as `T`: the way the ops set one field of a
/// contract struct by name, so the field list and the types are the struct's own.
///
/// # Errors
/// `invalid` on `field` for a name the object doesn't have (listing the ones it has, less `except`), or on `value`
/// for a value of the wrong type.
pub(crate) fn with_field<T: serde::Serialize + serde::de::DeserializeOwned>(
    object: &T,
    field: &str,
    to: &Value,
    except: &[&str],
) -> Result<T, OpError> {
    let mut value = serde_json::to_value(object).map_err(|e| OpError::internal(format!("serialising failed: {e}")))?;
    let Some(fields) = value.as_object_mut() else { return Err(OpError::internal("a struct serialises as an object")) };
    let names: Vec<String> = fields.keys().filter(|k| !except.contains(&k.as_str())).cloned().collect();
    if !names.iter().any(|n| n == field) {
        let near = crate::ops::part::describe::nearest(field, names.iter().map(String::as_str))
            .map_or_else(String::new, |n| format!("did you mean `{n}`? "));
        return Err(OpError::invalid("field", format!("`{field}` is not a field here"))
            .hint(format!("{near}the fields are: {}", names.join(", "))));
    }
    fields.insert(field.to_owned(), to.clone());
    serde_json::from_value(value).map_err(|e| {
        OpError::invalid("value", format!("`{field}` can't take {to}: {e}"))
            .hint("nothing was changed; part.describe lists each field's type")
    })
}
