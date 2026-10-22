//! A character's face as the preview ops paint it: the stored face (with defaults), its palette resolved, the head's face
//! window and its skin colour, read from the document the way the stage reads them from a build.

use omuzaki_char_api::{CharId, HeadParams, PartDef, paths};
use omuzaki_core::colour::Colour;
use omuzaki_doc_api::DocRead;
use omuzaki_face_api::{ColourRef, Palette};
use omuzaki_ops_api::OpError;
use serde::Deserialize;

use super::edit::effective;
use crate::preview::FaceSubject;

/// The skin a face is shown on when the character has no head part, or its skin slot is not in the palette: the head
/// kind's own default skin.
const FALLBACK_SKIN: Colour = Colour::rgb(0xf9, 0xd3, 0xc5);

/// The face of `character` as a preview paints it.
///
/// # Errors
/// `NotFound` without such a character; `Invalid` when its face, palette or head parameters don't read, or the face
/// names a palette slot the palette doesn't have.
pub(crate) fn subject(doc: &dyn DocRead, character: &CharId) -> Result<FaceSubject, OpError> {
    let Some(stored) = doc.get(&paths::character(character)) else {
        return Err(
            OpError::not_found(format!("there is no character '{character}'")).hint("`character.create` makes one")
        );
    };
    let face = effective(stored.get("face"))?;
    let palette = match stored.get("palette") {
        None => Palette::default(),
        Some(value) => Palette::deserialize(value).map_err(|e| {
            OpError::invalid("palette", format!("the palette in the document doesn't read: {e}"))
                .hint("fix the character file")
        })?,
    };
    let profile = face.resolve_colours(&palette).map_err(|e| {
        OpError::invalid("face", format!("the face's colours don't resolve: {e}"))
            .hint("add the slot to the palette, or give the face a fixed colour there")
    })?;
    let head = head_params(stored.get("parts"))?;
    let skin = match palette.resolve(&head.skin.base) {
        Ok(colour) => colour,
        Err(_) => match &head.skin.base {
            ColourRef::Fixed(colour) => *colour,
            ColourRef::Slot(_) => FALLBACK_SKIN,
        },
    };
    Ok(FaceSubject { profile, projection: head.face_projection(), skin: Colour { a: 255, ..skin } })
}

/// The parameters of the first enabled `head` part, or the head kind's defaults when there is none.
fn head_params(parts: Option<&serde_json::Value>) -> Result<HeadParams, OpError> {
    let Some(parts) = parts.and_then(serde_json::Value::as_object) else { return Ok(HeadParams::default()) };
    for (id, part) in parts {
        let Ok(def) = PartDef::deserialize(part) else { continue };
        if def.kind.as_str() != "head" || !def.enabled {
            continue;
        }
        return HeadParams::deserialize(&def.params).map_err(|e| {
            OpError::invalid(format!("parts/{id}/params"), format!("the head's parameters don't read: {e}"))
                .hint("fix the character file")
        });
    }
    Ok(HeadParams::default())
}
