//! The head's chin, which the layout ratios are read against. The head part sets it (`HeadParams::outline`), so a layout
//! op needs the character's head: a layout is never measured against a chin it was not given.

use omuzaki_char_api::{CharId, CharacterDef, HeadParams, PartDef, PartId};
use omuzaki_ops_api::OpError;

use crate::ops::character::access;

/// The part kind the head generator registers. A character's head is the part of this kind.
const HEAD_KIND: &str = "head";

/// The head-local height of the chin of `character`'s head.
///
/// # Errors
/// As [`head_of`].
pub(super) fn head_chin(def: &CharacterDef, character: &CharId) -> Result<f64, OpError> {
    head_of(def, character).map(|params| params.outline.chin_y())
}

/// The parameters of `character`'s head.
///
/// # Errors
/// `Invalid` naming `character` when it has no enabled head part, or more than one (a layout needs exactly one head);
/// `Invalid` naming `parts/<id>/params` when that head's parameters don't read as a head.
pub(super) fn head_of(def: &CharacterDef, character: &CharId) -> Result<HeadParams, OpError> {
    let enabled: Vec<(&PartId, &PartDef)> =
        def.parts.iter().filter(|(_, part)| part.kind.as_str() == HEAD_KIND && part.enabled).collect();
    let (id, part) = match enabled.as_slice() {
        [only] => *only,
        [] => {
            return Err(OpError::invalid(
                "character",
                format!(
                    "character `{character}` has no enabled head part, so its layout has no chin to measure against"
                ),
            )
            .hint("add a head part with `part.add` (kind `head`), or enable one with `part.set_enabled`"));
        }
        many => {
            let ids: Vec<&PartId> = many.iter().map(|(id, _)| *id).collect();
            return Err(OpError::invalid(
                "character",
                format!(
                    "character `{character}` has {} enabled head parts ({}), and a layout needs exactly one",
                    ids.len(),
                    access::list(ids)
                ),
            )
            .hint("disable all but one head with `part.set_enabled`"));
        }
    };
    head_params(id, part)
}

/// The head's parameters as the head kind reads them. Absent keys take their defaults, as the build does.
fn head_params(id: &PartId, part: &PartDef) -> Result<HeadParams, OpError> {
    serde_json::from_value(part.params.clone()).map_err(|e| {
        OpError::invalid(
            format!("parts/{id}/params"),
            format!("the head part `{id}` holds parameters that don't read as a head: {e}"),
        )
        .hint("repair the head's parameters in the project files, then retry")
    })
}

/// The error for a chin that is not below the eye line: no ratio can be read, and no layout can be set from one.
pub(super) fn chin_not_below_eye(eye_y: f64, chin_y: f64) -> OpError {
    OpError::invalid(
        "face/layout/eye_y",
        format!("the chin is at {chin_y} and the eye line at {eye_y}: the chin must be below the eye line"),
    )
    .hint("move the eye line down with `face.set_layout` (field `eye_y`), or lower the head's chin")
}
