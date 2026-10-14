//! Palette slots, resolved before painting (LK-03, gap G1).
//!
//! A [`FaceProfile`] may name colours by slot (`@eyes`), so a colourway swaps in one edit. A painter takes a profile
//! and nothing else: it has no palette, so a slot that reached it would paint wrong. [`FaceProfile::resolve_colours`]
//! returns a copy in which every slot is a fixed colour, and the stage calls it whenever the profile or the palette
//! changes. Painters never resolve colours themselves.
//!
//! The walk goes through JSON, so a colour field added to a profile later is resolved with no change here.

use std::collections::BTreeSet;

use omuzaki_core::colour::Colour;
use serde_json::Value;

use crate::palette::{ColourRef, Palette, PaletteSlot};
use crate::profile::FaceProfile;

/// Why a profile can't be resolved against a palette.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// A `@slot` that the palette doesn't define. Only the first one is reported, in JSON key order, so the same
    /// profile always names the same colour.
    #[error("{path}: the palette has no slot '{slot}'")]
    UnknownSlot {
        /// JSON path of the colour, `/`-separated: `eyes/iris/top`, `marks/0/colour`.
        path: String,
        /// The slot the palette lacks.
        slot: PaletteSlot,
    },
    /// The profile can't be written to JSON and read back. Only non-finite numbers do that, since JSON has no NaN.
    #[error("the profile can't be read back from JSON: {0}")]
    Json(String),
}

impl FaceProfile {
    /// A copy of this profile in which every `@slot` colour is the fixed colour the palette gives it. Nothing else
    /// changes, and resolving the copy again returns it unchanged.
    ///
    /// # Errors
    /// [`ResolveError::UnknownSlot`] for the first slot the palette lacks, and [`ResolveError::Json`] if the profile
    /// can't round-trip through JSON (non-finite numbers).
    pub fn resolve_colours(&self, palette: &Palette) -> Result<FaceProfile, ResolveError> {
        let mut tree = serde_json::to_value(self).map_err(|e| ResolveError::Json(e.to_string()))?;
        replace_slots(&mut tree, palette, "")?;
        serde_json::from_value(tree).map_err(|e| ResolveError::Json(e.to_string()))
    }

    /// Every palette slot this profile names, each once, in sorted order. The contrast checker and the palette UI
    /// read it. A profile always serialises (its maps have string keys), so the fallback to no slots is unreachable.
    pub fn slots_used(&self) -> BTreeSet<PaletteSlot> {
        let mut used = BTreeSet::new();
        let tree = serde_json::to_value(self).unwrap_or(Value::Null);
        collect_slots(&tree, &mut used);
        used
    }
}

/// Replaces each `@slot` string under `node` by its palette colour. `path` is where `node` sits, for the error.
fn replace_slots(node: &mut Value, palette: &Palette, path: &str) -> Result<(), ResolveError> {
    match node {
        Value::String(text) => {
            if let Some(colour) = palette_colour(text, palette, path)? {
                *node = Value::String(colour.to_string());
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                replace_slots(item, palette, &child_path(path, &i.to_string()))?;
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields.iter_mut() {
                replace_slots(value, palette, &child_path(path, key))?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

/// The palette colour for `text` when it is a `@slot` reference; `None` for fixed colours and other strings.
fn palette_colour(text: &str, palette: &Palette, path: &str) -> Result<Option<Colour>, ResolveError> {
    let Ok(ColourRef::Slot(slot)) = ColourRef::parse(text) else {
        return Ok(None);
    };
    match palette.0.get(&slot) {
        Some(colour) => Ok(Some(*colour)),
        None => Err(ResolveError::UnknownSlot { path: path.to_owned(), slot }),
    }
}

fn collect_slots(node: &Value, used: &mut BTreeSet<PaletteSlot>) {
    match node {
        Value::String(text) => {
            if let Ok(ColourRef::Slot(slot)) = ColourRef::parse(text) {
                used.insert(slot);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_slots(item, used)),
        Value::Object(fields) => fields.values().for_each(|value| collect_slots(value, used)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_owned() } else { format!("{path}/{key}") }
}
