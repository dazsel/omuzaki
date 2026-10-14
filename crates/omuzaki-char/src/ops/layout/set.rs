//! `face.set_layout` and `face.set_layout_ratios`: write the feature layout one field at a time, or all of it from ratios.

use omuzaki_char_api::CharId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::chin::{chin_not_below_eye, head_chin};
use super::edit::{changed_fields, layout_changes, refuse_bad_ratios, refuse_new_problems, with_field};
use super::ratios::LayoutRatios;
use crate::ops::character::access;

/// Sets one field of a character's feature layout (CB-12). The whole layout is validated and the edit is refused only for
/// a problem it introduces, naming the field and the range it accepts. A mouth must stay below the eye line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetLayout {
    /// The character whose layout changes.
    pub character: CharId,
    /// The field to set: `eye_y` (the eye line's height), `eye_spacing` (half the distance between the eyes), `eye_width`
    /// (half an eye's width), `brow_gap` (the brows above the eye line), `nose_y`, `mouth_y` or `blush_y` (the heights of
    /// the nose, mouth and blush), or `blush_spacing` (half the distance between the blush centres).
    pub field: String,
    /// The new value, in head-local metres for heights and lengths. Heights are negative below the head centre.
    pub value: f64,
}

impl Op for SetLayout {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_layout",
            summary: "Set one feature-layout field (eye line, eye spacing, nose, mouth or blush), validate the whole layout, \
                      and return the changed paths",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let current = def.face.layout;
        let after = with_field(&current, &self.field, self.value)?;
        refuse_new_problems(&current, &after)?;

        let fields = changed_fields(&current, &after)?;
        let changes = layout_changes(ctx, &self.character, &fields)?;
        let paths: Vec<String> = changes.iter().map(|c| c.path.to_string()).collect();
        Ok(access::edit_plan(
            changes,
            json!({ "character": self.character.to_string(), "field": self.field, "value": self.value, "changed": paths }),
        ))
    }
}

register_op!(SetLayout);

/// Sets a character's whole feature layout from ratios, the inverse of `face.layout.measure`. The eye line stays where it is,
/// the head's chin fixes the eye-to-chin drop, and the ratios give the eye distance and every feature's height.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetLayoutRatios {
    /// The character whose layout changes.
    pub character: CharId,
    /// The ratios to set, as `face.layout.measure` reports them under `ratios`. Each field has a range (fractions are from
    /// 0 to 1, the drop must be positive), and the chin must be below the eye line.
    pub ratios: LayoutRatios,
}

impl Op for SetLayoutRatios {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_layout_ratios",
            summary: "Set a character's feature layout from ratios (the inverse of face.layout.measure), keeping the eye \
                      line and taking the chin from the head; refuses a chin above the eye line",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        refuse_bad_ratios(&self.ratios, "ratios")?;
        let def = access::load(ctx, &self.character)?;
        let chin = head_chin(&def, &self.character)?;
        let current = def.face.layout;
        let target = self.ratios.layout_on(&current, chin).ok_or_else(|| chin_not_below_eye(current.eye_y, chin))?;
        refuse_new_problems(&current, &target)?;

        let fields = changed_fields(&current, &target)?;
        let changes = layout_changes(ctx, &self.character, &fields)?;
        let paths: Vec<String> = changes.iter().map(|c| c.path.to_string()).collect();
        Ok(access::edit_plan(
            changes,
            json!({ "character": self.character.to_string(), "chin_y": chin, "changed": paths }),
        ))
    }
}

register_op!(SetLayoutRatios);
