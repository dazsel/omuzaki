//! Reading a character's look node and writing it back, shared by the `look.*` ops.
//!
//! The ops read the character's definition only to check it exists, and this node; a sibling's variants, colourways or
//! rig are never read or written.

use omuzaki_char_api::{CharId, LookDef, LookError, LookParams, paths};
use omuzaki_ops_api::{OpCtx, OpError};

use crate::ops::character::access as character;
use crate::ops::character::access::not_found;

/// The look node of `character` (the default look when it has none), after checking the character exists.
///
/// # Errors
/// `not_found` (field `character`) for a missing character, `invalid` when the stored node doesn't match the look
/// format.
pub(crate) fn load(ctx: &OpCtx<'_>, id: &CharId) -> Result<LookDef, OpError> {
    character::load(ctx, id)?;
    LookDef::from_node(ctx.doc.get(&paths::look(id))).map_err(|e| {
        OpError::invalid("character", format!("the look of `{id}` is stored in a shape the look format rejects: {e}"))
            .hint("repair look.toml of the character, or replace it with look.set_preset")
    })
}

/// Resolves `look` (its preset and overrides) to check it can be drawn, before it is written.
///
/// # Errors
/// As [`refusal`], with `field` naming the argument at fault for a parameter out of range.
pub(crate) fn check(look: &LookDef, field: &str) -> Result<LookParams, OpError> {
    look.resolve().map_err(|e| refusal(&e, field))
}

/// The changes that store `look` as the look node of `id`: none when the node already holds it, so repeating an edit
/// is an empty patch. A look with no preset and no overrides is the default and leaves no node, so the files read as
/// they did before the first look edit.
///
/// # Errors
/// `internal` if the look can't be turned into a document value (every value the type holds can).
pub(crate) fn changes(ctx: &OpCtx<'_>, id: &CharId, look: &LookDef) -> Result<Vec<omuzaki_doc_api::Change>, OpError> {
    let after = if *look == LookDef::default() {
        None
    } else {
        Some(serde_json::to_value(look).map_err(|e| OpError::internal(format!("serialising the look failed: {e}")))?)
    };
    let change = character::change(ctx, paths::look(id), after);
    Ok(if change.before == change.after { Vec::new() } else { vec![change] })
}

/// The op error for a look error: an unknown preset is `not_found` on `preset` with the nearest id, a bad parameter
/// is `invalid` on `field`.
pub(crate) fn refusal(error: &LookError, field: &str) -> OpError {
    let message = error.to_string();
    match error {
        LookError::UnknownPreset { .. } => not_found("preset", message).hint("look.list_presets lists the presets"),
        LookError::Invalid { .. } => OpError::invalid(field, message)
            .hint("look.set_param takes a dotted path such as cloth.outline_width_scale; omit the value to clear one"),
        LookError::Source(_) | LookError::DuplicatePreset(_) => OpError::internal(message),
    }
}
