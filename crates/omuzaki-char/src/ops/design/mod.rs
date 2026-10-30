//! The design-layer ops (#2607 to #2615): named shape dials, age, sculpt strokes and material layers, every one an
//! edit of the character's `design` node (`characters/<id>/design`), so the UI, MCP, the CLI and scripts share them and
//! undo is the patch's inverse.
//!
//! | Module | Holds |
//! |---|---|
//! | [`dials`] | `shape.list_dials`, `shape.set_dial`, `shape.randomise`, `shape.reset` (#2612) |
//! | [`age`] | `character.set_age` (#2615) |
//! | [`sculpt`] | `part.sculpt.stroke`, `part.sculpt.undo`, `part.sculpt.clear`, `part.sculpt.list` (#2613) |
//! | [`layers`] | `part.layer.add`, `.remove`, `.move`, `.set`, `.mask`, `.list` and `part.decal.add` (#2609, #2610) |
//!
//! Each op writes the whole `design` node (or removes it when the edit leaves it empty), so the patch is one change and
//! a design the file leaves out stays left out.

use omuzaki_char_api::design::{DesignDef, DialDef, design_library};
use omuzaki_char_api::{CharId, CharacterDef, paths};
use omuzaki_doc_api::Change;
use omuzaki_ops_api::{OpCtx, OpError};

use crate::ops::character::access;

pub mod age;
pub mod dials;
pub mod layers;
pub mod sculpt;

/// The change that stores `design` as the character's design layer (removing the node when it is empty).
///
/// # Errors
/// `invalid` (field `character`) if the design breaks its own rules: the ops check their arguments first, so this
/// names a mistake in the stored design.
pub(crate) fn store(ctx: &OpCtx<'_>, character: &CharId, design: &DesignDef) -> Result<Vec<Change>, OpError> {
    design.check().map_err(|e| {
        OpError::invalid("character", format!("the design of `{character}` would be invalid: {e}"))
            .hint("fix the value the message names; shape.reset or part.sculpt.clear start a part over")
    })?;
    let after = if design.is_empty() {
        None
    } else {
        Some(serde_json::to_value(design).map_err(|e| OpError::internal(format!("can't store the design: {e}")))?)
    };
    let path = paths::design(character);
    if ctx.doc.get(&path).cloned() == after {
        return Ok(Vec::new());
    }
    Ok(vec![access::change(ctx, path, after)])
}

/// Every dial in the library.
///
/// # Errors
/// `internal` if the library doesn't load (the crate's tests rule that out).
pub(crate) fn dials() -> Result<&'static [DialDef], OpError> {
    design_library().map(|l| l.dials.as_slice()).map_err(|e| OpError::internal(format!("the dial library doesn't load: {e}")))
}

/// The dial `id`, or `not_found` (field `field`) with the nearest dial's id.
pub(crate) fn dial(id: &str, field: &str) -> Result<&'static DialDef, OpError> {
    let all = dials()?;
    all.iter().find(|d| d.id.as_str() == id).ok_or_else(|| {
        let hint = match omuzaki_core::text::nearest(id, all.iter().map(|d| d.id.as_str())) {
            Some(near) => format!("did you mean `{near}`? shape.list_dials lists all {}", all.len()),
            None => format!("shape.list_dials lists the {} dials", all.len()),
        };
        access::not_found(field, format!("there is no dial `{id}`")).hint(hint)
    })
}

/// The character and its design.
pub(crate) fn load(ctx: &OpCtx<'_>, character: &CharId) -> Result<(CharacterDef, DesignDef), OpError> {
    let def = access::load(ctx, character)?;
    let design = (*def.design).clone();
    Ok((def, design))
}
