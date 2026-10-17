//! `body.set_proportion`: one proportion slider of the character's body (CB-02, CB-03).

use omuzaki_char_api::{CharId, Proportions};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::params;
use crate::ops::character::access;

/// The proportion sliders, by the name an op takes them by.
const NAMES: [&str; 8] = [
    "neck_length",
    "torso_length",
    "shoulder_width",
    "hip_width",
    "arm_length",
    "leg_length",
    "hand_size",
    "foot_size",
];

/// The slider called `name`, if there is one.
fn slider<'a>(proportions: &'a mut Proportions, name: &str) -> Option<&'a mut f64> {
    match name {
        "neck_length" => Some(&mut proportions.neck_length),
        "torso_length" => Some(&mut proportions.torso_length),
        "shoulder_width" => Some(&mut proportions.shoulder_width),
        "hip_width" => Some(&mut proportions.hip_width),
        "arm_length" => Some(&mut proportions.arm_length),
        "leg_length" => Some(&mut proportions.leg_length),
        "hand_size" => Some(&mut proportions.hand_size),
        "foot_size" => Some(&mut proportions.foot_size),
        _ => None,
    }
}

/// Sets one proportion slider of a character's body. The slider is a multiplier on the template's size: `1` is the
/// template as it is, and the range is `0.5` to `2`. A value outside it is refused, naming the field and its range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "name": "leg_length", "value": 1.08}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetProportion {
    /// The character whose body changes.
    pub character: CharId,
    /// The slider: `neck_length`, `torso_length`, `shoulder_width`, `hip_width`, `arm_length`, `leg_length`, `hand_size`
    /// or `foot_size`.
    pub name: String,
    /// The multiplier, from `0.5` to `2`; `1` leaves the part as the template has it.
    pub value: f64,
}

impl Op for SetProportion {
    fn meta() -> OpMeta {
        OpMeta {
            name: "body.set_proportion",
            summary: "Set one body proportion slider (neck, torso, shoulder, hip, arm, leg, hand or foot size), refusing a \
                      value outside its range",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let body = params::load(&self.character, &def)?;
        let mut after = body.params.clone();
        let field = slider(&mut after.proportions, &self.name)
            .ok_or_else(|| params::unknown_name("name", "proportion slider", &self.name, &NAMES))?;
        let previous = *field;
        *field = self.value;
        params::refuse_invalid(&after)?;

        let changes = params::changes(ctx, &body.params_path, &body.params, &after)?;
        let touched = params::changed_paths(&changes);
        Ok(access::edit_plan(
            changes,
            json!({
                "character": self.character,
                "name": self.name,
                "previous": previous,
                "value": self.value,
                "changed": touched,
            }),
        ))
    }
}

register_op!(SetProportion);
