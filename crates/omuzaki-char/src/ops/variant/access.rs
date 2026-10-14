//! Reading a character's variants and resolving them, shared by the `variant.*` ops.
//!
//! The ops read a character's definition keys (through the character ops' `access::load`) and this sub-tree and
//! nothing else, so a sibling's colourways, rig or look are never read or written. Reading a variant's `extends`
//! chain touches only the variants it names, so one broken sibling doesn't stop an unrelated edit.

use omuzaki_char_api::{CharId, VariantDef, VariantError, VariantId, VariantSet, paths};
use omuzaki_doc_api::{DocError, DocPath, DocReadExt, Value};
use omuzaki_ops_api::{OpCtx, OpError};
use serde_json::Map;

use crate::ops::character::access::{list, not_found};

/// The variants of `character` as stored (the `variants` table), if it has any.
pub(crate) fn table<'a>(ctx: &OpCtx<'a>, character: &CharId) -> Option<&'a Map<String, Value>> {
    match ctx.doc.get(&paths::variants(character)) {
        Some(Value::Object(variants)) => Some(variants),
        _ => None,
    }
}

/// The `not_found` error for variant `id` of `character`, naming the variants that do exist. `field` is the argument
/// at fault: `variant` for the variant the caller named, `extends` for one it extends.
pub(crate) fn missing(ctx: &OpCtx<'_>, character: &CharId, id: &VariantId, field: &str) -> OpError {
    let known: Vec<String> = table(ctx, character).map(|v| v.keys().cloned().collect()).unwrap_or_default();
    let hint = if known.is_empty() {
        format!("`{character}` has no variants yet: create one with variant.create")
    } else {
        format!("variants of `{character}`: {}", list(known))
    };
    not_found(field, format!("there is no variant `{id}` on `{character}`")).hint(hint)
}

/// The variant `id` of `character` as stored.
///
/// # Errors
/// `not_found` (field `variant`) when it is missing, or `invalid` when the stored node does not match the variant
/// format.
pub(crate) fn load(ctx: &OpCtx<'_>, character: &CharId, id: &VariantId) -> Result<VariantDef, OpError> {
    match ctx.doc.get_as::<VariantDef>(&paths::variant(character, id)) {
        Ok(variant) => Ok(variant),
        Err(DocError::Missing(_)) => Err(missing(ctx, character, id, "variant")),
        Err(e) => Err(OpError::invalid(
            "variant",
            format!("variant `{id}` of `{character}` is stored in a shape the variant format rejects: {e}"),
        )
        .hint("repair the variant file (variants/<name>.toml), then retry")),
    }
}

/// The variants `first` extends, transitively, keyed by id. Each is read once; a loop stops the walk, and
/// `resolve_variant` reports the loop.
///
/// # Errors
/// As [`load`] for each variant on the chain.
pub(crate) fn ancestors(ctx: &OpCtx<'_>, character: &CharId, first: Option<&VariantId>) -> Result<VariantSet, OpError> {
    let mut set = VariantSet::new();
    let mut next = first.cloned();
    while let Some(id) = next.take() {
        if set.contains_key(&id) {
            break;
        }
        let variant = load(ctx, character, &id)?;
        next.clone_from(&variant.extends);
        set.insert(id, variant);
    }
    Ok(set)
}

/// `variant` under the id `id`, with the variants it extends: the set `resolve_variant` needs to resolve `id`. The
/// variant may be an edited copy of the stored one, so the set is what resolving the edit sees.
///
/// # Errors
/// As [`ancestors`].
pub(crate) fn with_ancestors(
    ctx: &OpCtx<'_>,
    character: &CharId,
    id: &VariantId,
    variant: VariantDef,
) -> Result<VariantSet, OpError> {
    let mut set = ancestors(ctx, character, variant.extends.as_ref())?;
    set.insert(id.clone(), variant);
    Ok(set)
}

/// The variants of `character` other than `id` that extend it, by name. Reads only each sibling's `extends` field, so
/// a sibling with other problems can't hide a dependant.
pub(crate) fn extending(ctx: &OpCtx<'_>, character: &CharId, id: &VariantId) -> Vec<String> {
    let Some(variants) = table(ctx, character) else { return Vec::new() };
    variants
        .iter()
        .filter(|(name, node)| {
            name.as_str() != id.as_str() && node.get("extends").and_then(Value::as_str) == Some(id.as_str())
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Where the overrides of variant `id` are stored. The table is replaced whole by an edit: its keys are whole paths
/// (`face/layout/mouth_y`), and a document path can't address one of them without splitting it at the `/`.
pub(crate) fn overrides_path(character: &CharId, id: &VariantId) -> DocPath {
    paths::variant(character, id).join("overrides")
}

/// The op error for a variant error. `field` names the argument at fault for an override that fails: the path
/// argument of `variant.set_override`, or the variant for the other ops.
pub(crate) fn refusal(error: &VariantError, field: &str) -> OpError {
    let message = error.to_string();
    match error {
        VariantError::Unknown(_) => not_found(field, message).hint("variant.list shows the variants of the character"),
        VariantError::Cycle(_) => OpError::invalid("extends", message)
            .hint("point `extends` at a variant that does not extend this one, so the chain ends"),
        VariantError::Override { .. } => OpError::invalid(field, message).hint(
            "correct the override with variant.set_override, or drop it with variant.clear_override; variant.preview \
             shows what the variant changes",
        ),
        VariantError::Serialise(_) => OpError::internal(message),
    }
}

/// Parses an override path from the arguments: non-empty, with `/` between names.
///
/// # Errors
/// `invalid` (field `path`) for an empty or malformed path, naming it.
pub(crate) fn parse_path(path: &str) -> Result<DocPath, OpError> {
    let parsed = DocPath::parse(path).map_err(|e| {
        OpError::invalid("path", format!("`{path}` is not an override path: {}", e.reason)).hint(
            "write the path from the character root with / between names, for example parts/head/params/outline/chin",
        )
    })?;
    if parsed.segments().is_empty() {
        return Err(OpError::invalid("path", "the override path is empty")
            .hint("name the field to override, for example face/layout/mouth_y"));
    }
    Ok(parsed)
}

/// Whether `value` holds a `null` anywhere. The project files can't store one (TOML has no null), so an edit refuses
/// it rather than writing a value that would not survive a save.
pub(crate) fn holds_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(holds_null),
        Value::Object(fields) => fields.values().any(holds_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}
