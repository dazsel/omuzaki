//! `face.layout.measure`: the layout as ratios (CB-12), side by side with reference ratios when the caller gives them.

use omuzaki_char_api::CharId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::chin::{chin_not_below_eye, head_of};
use super::edit::refuse_bad_ratios;
use super::ratios::{LayoutRatios, PartialLayoutRatios};
use crate::ops::character::access;

/// Measures a character's feature layout as ratios, the CB-12 overlay: the eyes' size and the eye-to-chin drop, the nose,
/// mouth and blush heights as fractions of that drop, and the blush spacing, all over the eye distance. Give `reference`
/// ratios (from the analysis of a reference image) and the answer adds the difference per feature and a plain-language
/// note for each. Ratios are read against the head's chin, so the character needs exactly one enabled head part.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct MeasureLayout {
    /// The character whose layout is measured.
    pub character: CharId,
    /// Reference ratios to compare with, in the same units: all six, or only some (the `layout_ratios` that
    /// `measure.feature_spacing` reads from a reference image), and the comparison covers the ones given. Left out, the
    /// answer has the character's ratios alone.
    #[serde(default)]
    pub reference: Option<PartialLayoutRatios>,
}

impl Op for MeasureLayout {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.layout.measure",
            summary: "Measure a character's feature layout as ratios (eye size, eye-to-chin drop, nose, mouth and blush \
                      heights) and the face's aspect (eye line to chin over the outline's width), optionally beside reference ratios \
                      with the difference and plain-language notes",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let outline = head_of(&def, &self.character)?.outline;
        let chin = outline.chin_y();
        let layout = def.face.layout;
        let ratios = LayoutRatios::measure(&layout, chin).ok_or_else(|| chin_not_below_eye(layout.eye_y, chin))?;
        // The face's proportion as the image measures read it (VF-01's `aspect`): the eye line to the chin over the
        // outline's width. A rounder face has a smaller aspect.
        let aspect = (layout.eye_y - chin) / (2.0 * outline.half_width).max(f64::EPSILON);

        let mut answer =
            json!({ "character": self.character.to_string(), "chin_y": chin, "aspect": aspect, "ratios": ratios });
        if let Some(reference) = &self.reference {
            if reference.is_empty() {
                return Err(OpError::invalid("reference", "the reference gives no ratio to compare with")
                    .hint("give at least one ratio, such as {\"drop_to_iod\": 1.6}, or leave the reference out"));
            }
            refuse_bad_ratios(reference, "reference")?;
            let (difference, notes) = reference.compare(&ratios);
            let fields = answer.as_object_mut().ok_or_else(|| OpError::internal("the answer is an object"))?;
            fields.insert("reference".to_owned(), json!(reference));
            fields.insert("difference".to_owned(), json!(difference));
            fields.insert("notes".to_owned(), Value::from(notes));
        }
        Ok(Plan::answer(answer))
    }
}

register_op!(MeasureLayout);
