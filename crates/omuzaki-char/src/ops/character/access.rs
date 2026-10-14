//! Reading a character out of the document and building the changes that edit it.
//!
//! The character ops and the part ops share these, so the wording of a missing character, part or kind is the same
//! everywhere and each op is only its own checks plus one patch.

use std::fmt::Display;

use omuzaki_char_api::{CharId, CharacterDef, PartDef, PartId, paths};
use omuzaki_doc_api::{Change, DocPath, Value};
use omuzaki_ops_api::{OpCtx, OpError, Plan};

/// The names joined for a message (``a, b, c``), or `none` when there are none.
pub(crate) fn list<T: Display>(items: impl IntoIterator<Item = T>) -> String {
    let joined = items.into_iter().map(|item| format!("`{item}`")).collect::<Vec<_>>().join(", ");
    if joined.is_empty() { "none".to_owned() } else { joined }
}

/// A `not_found` error that also names the argument at fault, so a caller knows which id to correct.
pub(crate) fn not_found(field: &str, message: impl Into<String>) -> OpError {
    let mut error = OpError::not_found(message);
    error.field = Some(field.to_owned());
    error
}

/// The ids of the characters in the document (`characters/*`), for "did you mean" lists. `any` is any character id:
/// the path of the `characters` table is taken from it rather than spelled here.
fn known_characters(ctx: &OpCtx<'_>, any: &CharId) -> Vec<String> {
    let table = paths::character(any).split_last().map_or_else(DocPath::root, |(table, _)| table);
    match ctx.doc.get(&table) {
        Some(Value::Object(characters)) => characters.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// The character `id` as stored in the document.
///
/// # Errors
/// `not_found` (field `character`) naming the characters that do exist, or `invalid` if the stored value does not
/// match the character format.
pub(crate) fn load(ctx: &OpCtx<'_>, id: &CharId) -> Result<CharacterDef, OpError> {
    let Some(node) = ctx.doc.get(&paths::character(id)) else {
        let known = known_characters(ctx, id);
        let hint = if known.is_empty() {
            "the project has no characters yet: create one with character.create".to_owned()
        } else {
            format!("characters in this project: {}", list(known))
        };
        return Err(not_found("character", format!("there is no character `{id}`")).hint(hint));
    };
    CharacterDef::from_node(node).map_err(|e| {
        OpError::invalid(
            "character",
            format!("character `{id}` is stored in a shape the character format rejects: {e}"),
        )
        .hint("repair the character in the project files (its parts must match the part format), then retry")
    })
}

/// The part `id` of `def`, which is the character `character`.
///
/// # Errors
/// `not_found` (field `field`) naming the parts the character has.
pub(crate) fn part<'a>(
    def: &'a CharacterDef,
    character: &CharId,
    id: &PartId,
    field: &str,
) -> Result<&'a PartDef, OpError> {
    def.parts.get(id).ok_or_else(|| {
        not_found(field, format!("character `{character}` has no part `{id}`"))
            .hint(format!("the parts of `{character}` are: {}", list(def.parts.keys())))
    })
}

/// The change that sets `path` to `after` (`None` removes it), expecting what the document holds there now. Reading
/// `before` from the document rather than from the typed character keeps undo exact when a file leaves out a
/// default (`enabled`, `layer`).
pub(crate) fn change(ctx: &OpCtx<'_>, path: DocPath, after: Option<Value>) -> Change {
    Change { before: ctx.doc.get(&path).cloned(), path, after }
}

/// The changes that create the node at `path` holding `value`, preceded by one that creates its parent table when the
/// document lacks it (the first character of a project, a character stored without a `parts` table). A change needs
/// its parent to exist, and undo must remove everything the edit created, so the table is part of the same patch.
pub(crate) fn create(ctx: &OpCtx<'_>, path: DocPath, value: Value) -> Vec<Change> {
    let mut changes = Vec::new();
    if let Some((table, _)) = path.split_last()
        && ctx.doc.get(&table).is_none()
    {
        changes.push(Change { path: table, before: None, after: Some(Value::Object(serde_json::Map::new())) });
    }
    changes.push(Change { path, before: None, after: Some(value) });
    changes
}

/// An edit's plan: the patch, and a summary of the result for the caller (shown in history and to agents). A summary
/// field that is `null` (an option that is `None`: no parent, no note) is left out rather than shown as `null`.
pub(crate) fn edit_plan(changes: Vec<Change>, summary: Value) -> Plan {
    let summary = match summary {
        Value::Object(mut fields) => {
            fields.retain(|_, field| !field.is_null());
            Value::Object(fields)
        }
        other => other,
    };
    Plan { output: Some(summary), ..Plan::edit(omuzaki_doc_api::Patch { changes }) }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_summary_leaves_out_its_null_fields() {
        let plan =
            edit_plan(Vec::new(), json!({"part": "hood", "parent": null, "build_order": ["body"], "note": null}));
        assert_eq!(plan.output, Some(json!({"part": "hood", "build_order": ["body"]})));
    }

    #[test]
    fn lists_quote_the_names_and_say_when_there_are_none() {
        assert_eq!(list(["a", "b"]), "`a`, `b`");
        assert_eq!(list(Vec::<String>::new()), "none");
    }
}
