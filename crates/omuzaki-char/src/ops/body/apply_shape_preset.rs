//! `body.apply_shape_preset`: blends a body-shape preset into the character's current shape (CB-04).

use crate::template::shape::presets;
use omuzaki_char_api::CharId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::params;
use crate::ops::character::access;

/// Blends a preset of the body-shape library into the character's current shape. `blend` `0` keeps the shape as it is
/// and `1` takes the preset outright; in between, each axis moves that fraction of the way to the preset. The result
/// keeps the preset's name only at the ends (or when the shape is already that preset).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyShapePreset {
    /// The character whose body changes.
    pub character: CharId,
    /// The preset from the library: `average`, `slim`, `broad`, `curvy`, `athletic`, `stocky`, `pear`, `hourglass`,
    /// `lean-tall` or `petite`.
    pub preset: String,
    /// How far to move towards the preset, from `0` (not at all) to `1` (all the way).
    pub blend: f64,
}

impl Op for ApplyShapePreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "body.apply_shape_preset",
            summary: "Blend a body-shape preset into the character's current shape by a fraction from 0 to 1",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(0.0..=1.0).contains(&self.blend) {
            return Err(OpError::invalid("blend", format!("{} is out of range: use a number from 0 to 1", self.blend))
                .hint("0 keeps the current shape and 1 takes the preset"));
        }
        let library = presets().map_err(|e| OpError::internal(e.to_string()))?;
        let names: Vec<&str> = library.iter().map(|p| p.id.as_str()).collect();
        let preset = library
            .iter()
            .find(|p| p.id.as_str() == self.preset)
            .ok_or_else(|| params::unknown_name("preset", "body-shape preset", &self.preset, &names))?;

        let def = access::load(ctx, &self.character)?;
        let body = params::load(&self.character, &def)?;
        let mut after = body.params.clone();
        after.shape = body.params.shape.lerp(&preset.shape, self.blend);
        params::refuse_invalid(&after)?;

        let changes = params::changes(ctx, &body.params_path, &body.params, &after)?;
        let touched = params::changed_paths(&changes);
        Ok(access::edit_plan(
            changes,
            json!({
                "character": self.character,
                "preset": self.preset,
                "blend": self.blend,
                "changed": touched,
            }),
        ))
    }
}

register_op!(ApplyShapePreset);
