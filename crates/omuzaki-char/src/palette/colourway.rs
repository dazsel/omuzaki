//! Colourways (LK-03): a saved palette that a character switches to in one edit.
//!
//! A colourway is a whole palette kept beside the live one, at `characters/<id>/colourways/<name>`. Parts refer to
//! colours by slot, so applying one changes only the palette's colours: `colourway.apply` replaces them in one patch
//! over `palette/`, and undo restores them exactly. The slots themselves stay. A colourway that does not name a slot
//! leaves that colour alone, and one that names a slot the palette lacks does not create it.

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, ColourwayId, paths};
use omuzaki_core::colour::Colour;
use omuzaki_doc_api::{Change, Value};
use omuzaki_face_api::{Palette, PaletteSlot};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::ops::slot_path;
use crate::ops::character::access::{self, list};

/// What applying a colourway does to a palette, slot by slot.
#[derive(Debug, Default, PartialEq)]
struct Swap {
    /// Slots whose colour changes, with the colour before and the colour after.
    changed: Vec<(PaletteSlot, Colour, Colour)>,
    /// Palette slots the colourway does not name: they keep their colour.
    not_in_colourway: Vec<PaletteSlot>,
    /// Slots the colourway names that the palette lacks: they are not added.
    not_in_palette: Vec<PaletteSlot>,
}

impl Swap {
    /// The swap that takes `live` to the colours of `saved`.
    fn between(live: &Palette, saved: &Palette) -> Self {
        let mut swap = Self::default();
        for (slot, colour) in &live.0 {
            match saved.0.get(slot) {
                Some(next) if next != colour => swap.changed.push((slot.clone(), *colour, *next)),
                Some(_) => {}
                None => swap.not_in_colourway.push(slot.clone()),
            }
        }
        swap.not_in_palette = saved.0.keys().filter(|slot| !live.0.contains_key(*slot)).cloned().collect();
        swap
    }

    /// The patch that makes the changes: one change per slot whose colour changes, over `palette/` only.
    fn patch(&self, ctx: &OpCtx<'_>, character: &CharId) -> Vec<Change> {
        self.changed
            .iter()
            .map(|(slot, _, next)| access::change(ctx, slot_path(character, slot), Some(json!(next))))
            .collect()
    }

    /// The swap as the op's output, the same for a preview and for an apply.
    fn report(&self, character: &CharId, colourway: &ColourwayId) -> Value {
        let changed: Vec<Value> = self
            .changed
            .iter()
            .map(|(slot, previous, colour)| json!({ "slot": slot, "previous": previous, "colour": colour }))
            .collect();
        json!({
            "character": character,
            "colourway": colourway,
            "changed": changed,
            "not_in_colourway": self.not_in_colourway,
            "not_in_palette": self.not_in_palette,
        })
    }
}

/// Every colourway the character has saved, by name, as the document holds it. This is the one reader of the
/// `colourways` table: the colourway ops and `palette.rename_slot` all go through it, so they see the same entries.
pub(crate) fn saved(ctx: &OpCtx<'_>, character: &CharId) -> BTreeMap<String, Value> {
    match ctx.doc.get(&paths::colourways(character)) {
        Some(Value::Object(saved)) => saved.iter().map(|(name, palette)| (name.clone(), palette.clone())).collect(),
        _ => BTreeMap::new(),
    }
}

/// The stored colourway `name`, as the document holds it.
///
/// # Errors
/// `not_found` (field `name`) listing the colourways the character has.
fn stored<'a>(ctx: &'a OpCtx<'_>, character: &CharId, name: &ColourwayId) -> Result<&'a Value, OpError> {
    ctx.doc.get(&paths::colourway(character, name)).ok_or_else(|| {
        access::not_found("name", format!("character `{character}` has no colourway `{name}`")).hint(format!(
            "its colourways are: {}; `colourway.save` saves the current palette as one",
            list(saved(ctx, character).into_keys())
        ))
    })
}

/// The palette of colourway `name`.
///
/// # Errors
/// `not_found` if there is no such colourway, `invalid` (field `name`) if it does not read as a palette.
fn load_colourway(ctx: &OpCtx<'_>, character: &CharId, name: &ColourwayId) -> Result<Palette, OpError> {
    let value = stored(ctx, character, name)?;
    serde_json::from_value(value.clone()).map_err(|e| {
        OpError::invalid("name", format!("colourway `{name}` is stored in a shape the palette format rejects: {e}"))
            .hint("repair the colourway in the project files, then retry")
    })
}

/// Saves the character's current palette as a colourway, so it can be applied later.
///
/// The name must be new: a colourway is replaced by deleting it first, so nothing is overwritten by accident. The
/// colourway holds every colour of the palette now; later palette edits do not change it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColourwaySave {
    /// The character.
    pub character: CharId,
    /// The name to save under, for example `winter`.
    pub name: ColourwayId,
}

impl Op for ColourwaySave {
    fn meta() -> OpMeta {
        OpMeta {
            name: "colourway.save",
            summary: "Save the character's current palette as a named colourway, to apply again later.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let path = paths::colourway(&self.character, &self.name);
        if ctx.doc.get(&path).is_some() {
            return Err(OpError::invalid(
                "name",
                format!("character `{}` already has a colourway `{}`", self.character, self.name),
            )
            .hint("delete it with colourway.delete first, or save under another name"));
        }
        let palette = serde_json::to_value(&def.palette)
            .map_err(|e| OpError::internal(format!("serialising the palette failed: {e}")))?;
        let changes = access::create(ctx, path, palette);
        let summary = json!({ "character": self.character, "saved": self.name, "slots": def.palette.0.len() });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(ColourwaySave);

/// Switches the character to a saved colourway: each palette slot the colourway names takes its colour, in one
/// edit. Parts are not touched, and undo restores the previous colours.
///
/// Only colours change. A slot the colourway does not name keeps its colour, and a colour the colourway names for a
/// slot the palette lacks is not added. `colourway.preview` shows the same changes without applying them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColourwayApply {
    /// The character.
    pub character: CharId,
    /// The colourway to switch to.
    pub name: ColourwayId,
}

impl Op for ColourwayApply {
    fn meta() -> OpMeta {
        OpMeta {
            name: "colourway.apply",
            summary: "Switch to a saved colourway: replace the palette colours in one edit; parts and slots stay.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let saved = load_colourway(ctx, &self.character, &self.name)?;
        let swap = Swap::between(&def.palette, &saved);
        let summary = swap.report(&self.character, &self.name);
        Ok(access::edit_plan(swap.patch(ctx, &self.character), summary))
    }
}

register_op!(ColourwayApply);

/// Deletes a saved colourway from the character. Undo brings it back exactly, with every colour it held, so a deletion
/// never loses a palette the user saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColourwayDelete {
    /// The character.
    pub character: CharId,
    /// The colourway to delete.
    pub name: ColourwayId,
}

impl Op for ColourwayDelete {
    fn meta() -> OpMeta {
        OpMeta {
            name: "colourway.delete",
            summary: "Delete a saved colourway; undo restores it.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        access::load(ctx, &self.character)?;
        stored(ctx, &self.character, &self.name)?;
        let changes = vec![access::change(ctx, paths::colourway(&self.character, &self.name), None)];
        let summary = json!({ "character": self.character, "deleted": self.name });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(ColourwayDelete);

/// Lists the character's saved colourways by name, each with the colours it holds, so an agent can pick one to preview
/// or apply. Read-only: nothing in the document changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColourwayList {
    /// The character.
    pub character: CharId,
}

impl Op for ColourwayList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "colourway.list",
            summary: "List the character's saved colourways, by name, with the colours each one holds.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        access::load(ctx, &self.character)?;
        let listed: Vec<Value> = saved(ctx, &self.character)
            .into_iter()
            .map(|(name, palette)| json!({ "name": name, "palette": palette }))
            .collect();
        Ok(Plan::answer(json!({ "character": self.character, "colourways": listed })))
    }
}

register_op!(ColourwayList);

/// Shows what applying a colourway would change, without applying it: the slots whose colour changes, the slots the
/// colourway does not name, and the slots it names that the palette lacks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColourwayPreview {
    /// The character.
    pub character: CharId,
    /// The colourway to preview.
    pub name: ColourwayId,
}

impl Op for ColourwayPreview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "colourway.preview",
            summary: "Preview a colourway: the palette slots that would change colour, and the ones it leaves alone.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let saved = load_colourway(ctx, &self.character, &self.name)?;
        let swap = Swap::between(&def.palette, &saved);
        Ok(Plan::answer(swap.report(&self.character, &self.name)))
    }
}

register_op!(ColourwayPreview);
