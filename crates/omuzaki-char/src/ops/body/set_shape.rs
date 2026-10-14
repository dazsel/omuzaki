//! `body.set_shape`: one axis of the body-shape space (CB-04).

use omuzaki_char_api::{BodyShape, CharId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::params;
use crate::ops::character::access;

/// The five axes, by the name an op takes them by.
const AXES: [&str; 5] = ["height", "build", "curves", "musculature", "shoulder_hip"];

/// The axis called `axis`, if there is one.
fn axis<'a>(shape: &'a mut BodyShape, name: &str) -> Option<&'a mut f64> {
    match name {
        "height" => Some(&mut shape.height),
        "build" => Some(&mut shape.build),
        "curves" => Some(&mut shape.curves),
        "musculature" => Some(&mut shape.musculature),
        "shoulder_hip" => Some(&mut shape.shoulder_hip),
        _ => None,
    }
}

/// Sets one axis of a character's body-shape space. `-1` and `+1` are the ends of the axis and `0` is the template as
/// it is; a value outside `-1` to `1` is refused, naming the field and its range. The other axes are not touched, so
/// the preset the shape was derived from stays recorded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetShape {
    /// The character whose body changes.
    pub character: CharId,
    /// The axis: `height` (shorter to taller), `build` (slim to broad), `curves` (straight to curvy), `musculature` (soft
    /// to heavy) or `shoulder_hip` (hip-heavy to shoulder-heavy).
    pub axis: String,
    /// The new value, from `-1` to `1`; `0` is the template.
    pub value: f64,
}

impl Op for SetShape {
    fn meta() -> OpMeta {
        OpMeta {
            name: "body.set_shape",
            summary: "Set one body-shape axis (height, build, curves, musculature or shoulder/hip), refusing a value outside \
                      -1 to 1",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let body = params::load(&self.character, &def)?;
        let mut after = body.params.clone();
        let field = axis(&mut after.shape, &self.axis)
            .ok_or_else(|| params::unknown_name("axis", "body-shape axis", &self.axis, &AXES))?;
        let previous = *field;
        *field = self.value;
        params::refuse_invalid(&after)?;

        let changes = params::changes(ctx, &body.params_path, &body.params, &after)?;
        let touched = params::changed_paths(&changes);
        Ok(access::edit_plan(
            changes,
            json!({
                "character": self.character,
                "axis": self.axis,
                "previous": previous,
                "value": self.value,
                "changed": touched,
            }),
        ))
    }
}

register_op!(SetShape);
