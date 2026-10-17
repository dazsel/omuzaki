//! The palette ops (LK-03): `palette.set`, `palette.add_slot`, `palette.remove_slot`, `palette.rename_slot` and
//! `palette.usage`.
//!
//! A character's palette maps slot names to fixed colours. Parts and the face name a slot as `@slot`, so changing a
//! slot's colour recolours everything that uses it without touching the parts. Each op plans one patch, and undo is
//! that patch's inverse, so a rename moves the palette key and every reference to it together.
//!
//! The refusals are the ones that would leave a dangling `@slot`: removing a slot something still names (the error
//! lists where), and renaming into a name that something already names without the palette defining it.
//!
//! Limits. The references these ops see are the ones stored in the parts' params and the face. A colour a
//! part takes from its own default, which the file leaves out (the head's and body's skin), and a variant's
//! override that names a slot, are not seen. So removing or renaming a slot can leave one of them naming a slot
//! that no longer exists. Both are filed; check a character's defaults and variants before you remove or rename.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_char_api::{CharId, CharacterDef, paths};
use omuzaki_core::colour::Colour;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_face_api::{ColourRef, PaletteSlot};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};

use super::colourway;
use crate::ops::character::access::{self, list};

/// Where each palette slot is named: the document paths of its references.
type Uses = BTreeMap<PaletteSlot, Vec<DocPath>>;

/// The path of palette slot `slot` of `character`.
pub(crate) fn slot_path(character: &CharId, slot: &PaletteSlot) -> DocPath {
    paths::palette(character).join(slot.as_str())
}

/// A fixed colour from its text (`#rrggbb` or `#rrggbbaa`), which is what a palette stores.
///
/// # Errors
/// `invalid` naming `field`, with the accepted forms and the hint that `@slot` belongs in a part, not a palette.
fn fixed_colour(field: &str, text: &str) -> Result<Colour, OpError> {
    text.parse::<Colour>().map_err(|_| {
        OpError::invalid(field, format!("`{text}` isn't a colour: use #rrggbb or #rrggbbaa"))
            .hint("a palette holds fixed colours; a part or the face refers to a slot with @slot_name")
    })
}

/// The colour of `slot` in the character's palette.
///
/// # Errors
/// `not_found` (naming the argument `field` that gave the slot) listing the palette's slots, with the hint that
/// `palette.add_slot` creates one.
fn current<'a>(
    def: &'a CharacterDef,
    character: &CharId,
    slot: &PaletteSlot,
    field: &str,
) -> Result<&'a Colour, OpError> {
    def.palette.0.get(slot).ok_or_else(|| {
        access::not_found(field, format!("character `{character}` has no palette slot `{slot}`"))
            .hint(format!("its slots are: {}; `palette.add_slot` creates one", list(def.palette.0.keys())))
    })
}

/// Every `@slot` colour reference under `value`, each with the path it sits at (`at` is where `value` is).
fn references_in(value: &Value, at: &DocPath, found: &mut Vec<(PaletteSlot, DocPath)>) {
    match value {
        Value::String(text) => {
            if let Ok(ColourRef::Slot(slot)) = ColourRef::parse(text) {
                found.push((slot, at.clone()));
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                references_in(item, &at.join(i.to_string()), found);
            }
        }
        Value::Object(fields) => {
            for (key, item) in fields {
                references_in(item, &at.join(key.as_str()), found);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// The references in every part's params, read from the document: the stored text, so an edit's `before` matches it.
fn part_references(ctx: &OpCtx<'_>, character: &CharId, def: &CharacterDef) -> Vec<(PaletteSlot, DocPath)> {
    let mut found = Vec::new();
    for part in def.parts.keys() {
        let at = paths::part_params(character, part);
        if let Some(params) = ctx.doc.get(&at) {
            references_in(params, &at, &mut found);
        }
    }
    found
}

/// The references in the stored face, leaf by leaf.
fn face_references(ctx: &OpCtx<'_>, character: &CharId) -> Vec<(PaletteSlot, DocPath)> {
    let at = paths::face(character);
    let mut found = Vec::new();
    if let Some(face) = ctx.doc.get(&at) {
        references_in(face, &at, &mut found);
    }
    found
}

/// Where each palette slot is named: at each reference in the parts, and at the exact field of each face leaf that
/// names it. A slot the face reports through `FaceProfile::slots_used` but the stored leaves do not name (a default the
/// file leaves out) is listed at the face root, so that use is never hidden.
fn usage(ctx: &OpCtx<'_>, character: &CharId, def: &CharacterDef) -> Uses {
    let face = face_references(ctx, character);
    let in_face: BTreeSet<PaletteSlot> = face.iter().map(|(slot, _)| slot.clone()).collect();
    let mut uses = Uses::new();
    for (slot, path) in part_references(ctx, character, def).into_iter().chain(face) {
        uses.entry(slot).or_default().push(path);
    }
    for slot in def.face.slots_used() {
        if !in_face.contains(&slot) {
            uses.entry(slot).or_default().push(paths::face(character));
        }
    }
    uses
}

/// The paths as text, for an op's output and its messages.
fn texts(locations: &[DocPath]) -> Vec<String> {
    locations.iter().map(ToString::to_string).collect()
}

/// Sets the colour of a palette slot that already exists. Every part and the face that names the slot takes the new
/// colour, and nothing else changes.
///
/// The slot must exist: a new name is created only by `palette.add_slot`, so a typo cannot quietly add a colour. The
/// colour is a fixed `#rrggbb` or `#rrggbbaa`. Setting a slot to the colour it already has changes nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "slot": "skin", "colour": "#f6d2c4"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PaletteSet {
    /// The character.
    pub character: CharId,
    /// The slot to recolour, for example `hair_root`.
    pub slot: PaletteSlot,
    /// The new colour, `#rrggbb` or `#rrggbbaa`.
    pub colour: String,
}

impl Op for PaletteSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "palette.set",
            summary: "Set the colour of an existing palette slot; every part and face that names it recolours.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let colour = fixed_colour("colour", &self.colour)?;
        let previous = *current(&def, &self.character, &self.slot, "slot")?;
        let differs = colour != previous;
        let changes = if differs {
            vec![access::change(ctx, slot_path(&self.character, &self.slot), Some(json!(colour)))]
        } else {
            Vec::new()
        };
        let summary = json!({
            "character": self.character,
            "slot": self.slot,
            "previous": previous,
            "colour": colour,
            "changed": differs,
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PaletteSet);

/// Adds a palette slot with a colour. The slot must be new: `palette.set` changes one that exists.
///
/// A part or the face may already name the slot (a reference the palette did not define): adding it makes that
/// reference resolve. The colour is a fixed `#rrggbb` or `#rrggbbaa`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "slot": "accent", "colour": "#e07a9a"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PaletteAddSlot {
    /// The character.
    pub character: CharId,
    /// The new slot's name, for example `blush`.
    pub slot: PaletteSlot,
    /// The colour, `#rrggbb` or `#rrggbbaa`.
    pub colour: String,
}

impl Op for PaletteAddSlot {
    fn meta() -> OpMeta {
        OpMeta {
            name: "palette.add_slot",
            summary: "Add a palette slot with a colour, so parts and the face can name it as @slot.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let colour = fixed_colour("colour", &self.colour)?;
        if let Some(existing) = def.palette.0.get(&self.slot) {
            return Err(OpError::invalid(
                "slot",
                format!("character `{}` already has palette slot `{}` ({existing})", self.character, self.slot),
            )
            .hint("change its colour with palette.set"));
        }
        let changes = access::create(ctx, slot_path(&self.character, &self.slot), json!(colour));
        let summary = json!({ "character": self.character, "slot": self.slot, "colour": colour });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PaletteAddSlot);

/// Removes a palette slot. Refused while a part or the face still names it: the error lists where, and the slot can go
/// once nothing refers to it. Undo brings the slot back with its colour.
///
/// Limits: the uses it sees are the references stored in the parts' params and the face. A colour a part
/// takes from its own default (the head's skin when the file leaves it out) and a variant's override are
/// not seen, so removing the slot can strand one of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "slot": "accent"}
    ],
    "x-omuzaki-fixture" = "character-slot"
))]
pub struct PaletteRemoveSlot {
    /// The character.
    pub character: CharId,
    /// The slot to remove.
    pub slot: PaletteSlot,
}

impl Op for PaletteRemoveSlot {
    fn meta() -> OpMeta {
        OpMeta {
            name: "palette.remove_slot",
            summary: "Remove a palette slot; refused, listing where it is used, while a part or the face names it.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let colour = *current(&def, &self.character, &self.slot, "slot")?;
        let used = usage(ctx, &self.character, &def).remove(&self.slot).unwrap_or_default();
        if !used.is_empty() {
            return Err(OpError::invalid(
                "slot",
                format!("can't remove palette slot `{}`: it is used by {}", self.slot, list(texts(&used))),
            )
            .hint(
                "point those uses at a fixed colour or another slot first, by editing the part or the face; \
                   palette.rename_slot only moves them to a new name. Defaults and variant overrides are not \
                   listed",
            ));
        }
        let changes = vec![access::change(ctx, slot_path(&self.character, &self.slot), None)];
        let summary = json!({ "character": self.character, "removed": self.slot, "colour": colour });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PaletteRemoveSlot);

/// Renames a palette slot and every reference to it, in one patch: the palette key, each part and face reference
/// `@from`, and the same key in each saved colourway. The renamed character builds exactly as it did.
///
/// The new name must be free, and nothing may already name it: a reference to a slot the palette lacks would silently
/// start resolving to this slot's colour. Limits: as for `palette.remove_slot`, a default or a variant
/// override that names `@from` is not rewritten.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "from": "accent", "to": "highlight"}
    ],
    "x-omuzaki-fixture" = "character-slot"
))]
pub struct PaletteRenameSlot {
    /// The character.
    pub character: CharId,
    /// The slot's current name.
    pub from: PaletteSlot,
    /// The new name: the palette must not have it yet, and no part or face may already name it.
    pub to: PaletteSlot,
}

impl Op for PaletteRenameSlot {
    fn meta() -> OpMeta {
        OpMeta {
            name: "palette.rename_slot",
            summary: "Rename a palette slot and rewrite every @from reference in the parts, the face and the \
                      colourways, in one edit.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        current(&def, &self.character, &self.from, "from")?;
        if def.palette.0.contains_key(&self.to) {
            return Err(OpError::invalid(
                "to",
                format!("character `{}` already has palette slot `{}`", self.character, self.to),
            )
            .hint("choose a name the palette doesn't use yet, or remove that slot first"));
        }
        let named: Vec<(PaletteSlot, DocPath)> = part_references(ctx, &self.character, &def)
            .into_iter()
            .chain(face_references(ctx, &self.character))
            .collect();
        let naming_to: Vec<DocPath> =
            named.iter().filter(|(slot, _)| *slot == self.to).map(|(_, path)| path.clone()).collect();
        if !naming_to.is_empty() {
            return Err(OpError::invalid(
                "to",
                format!(
                    "character `{}` has no palette slot `{}`, but {} already names it",
                    self.character,
                    self.to,
                    list(texts(&naming_to))
                ),
            )
            .hint(format!(
                "add `{}` with palette.add_slot first if that colour is meant, or pick a name nothing refers to yet",
                self.to
            )));
        }

        let from_path = slot_path(&self.character, &self.from);
        let mut changes = vec![
            access::change(ctx, from_path.clone(), None),
            access::change(ctx, slot_path(&self.character, &self.to), ctx.doc.get(&from_path).cloned()),
        ];

        let saved = colourway::saved(ctx, &self.character);
        for (name, palette) in &saved {
            if let Some(colour) = palette.get(self.from.as_str()) {
                let at = paths::colourways(&self.character).join(name.as_str());
                changes.push(access::change(ctx, at.join(self.from.as_str()), None));
                changes.push(access::change(ctx, at.join(self.to.as_str()), Some(colour.clone())));
            }
        }

        let mut references = Vec::new();
        for (_, path) in named.into_iter().filter(|(slot, _)| *slot == self.from) {
            changes.push(access::change(ctx, path.clone(), Some(json!(format!("@{}", self.to)))));
            references.push(path);
        }

        let renamed_in: Vec<&String> = saved
            .iter()
            .filter(|(_, palette)| palette.get(self.from.as_str()).is_some())
            .map(|(name, _)| name)
            .collect();
        let summary = json!({
            "character": self.character,
            "from": self.from,
            "to": self.to,
            "references": texts(&references),
            "colourways": renamed_in,
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PaletteRenameSlot);

/// Lists every palette slot with its colour and the places that name it, so a recolour or a removal can be judged.
/// A reference to a slot the palette lacks is listed under `undefined`. Defaults a file leaves out and variant
/// overrides are not listed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PaletteUsage {
    /// The character.
    pub character: CharId,
}

impl Op for PaletteUsage {
    fn meta() -> OpMeta {
        OpMeta {
            name: "palette.usage",
            summary: "List each palette slot with its colour and the parts, face paths and references that use it.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let uses = usage(ctx, &self.character, &def);
        let slots: Map<String, Value> = def
            .palette
            .0
            .iter()
            .map(|(slot, colour)| {
                let used_by = uses.get(slot).map(|at| texts(at)).unwrap_or_default();
                (slot.to_string(), json!({ "colour": colour, "used_by": used_by }))
            })
            .collect();
        let undefined: Map<String, Value> = uses
            .iter()
            .filter(|(slot, _)| !def.palette.0.contains_key(*slot))
            .map(|(slot, at)| (slot.to_string(), json!(texts(at))))
            .collect();
        Ok(Plan::answer(json!({ "character": self.character, "slots": slots, "undefined": undefined })))
    }
}

register_op!(PaletteUsage);
