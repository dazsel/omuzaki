//! `look.set_param`: sets or clears one override of a character's look.

use omuzaki_char_api::{CharId, LookParams};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use crate::ops::character::access as character;

/// Sets one override of a character's look, on top of its preset, or clears it so the preset's value shows through.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "param": "cloth.outline_width_scale", "value": 1.2}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct LookSetParam {
    /// The character to restyle.
    pub character: CharId,
    /// The parameter, as a dotted path: `<section>.<field>` where the section is `all`, a material class (`skin`,
    /// `hair`, `cloth`, `metal`, `eyes`) or `scene`. For example `cloth.outline_width_scale`, `all.softness_scale`,
    /// `scene.bloom`, `hair.program_params.hair.ring-strength`.
    pub param: String,
    /// The new value, as the look file holds it: `0.5`, `true`, `"material"`, `{"flat": "#2a1f1a"}`,
    /// `{"float": 0.0}` for a program parameter. Omit it (or pass null) to clear the override.
    #[serde(default)]
    pub value: Option<Value>,
}

impl Op for LookSetParam {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.set_param",
            summary: "Set one parameter of a character's look over its preset (cloth.outline_width_scale, scene.bloom, \
                      ...), or clear it so the preset's value shows through.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut look = access::load(ctx, &self.character)?;
        LookParams::check_path(&self.param).map_err(|e| access::refusal(&e, "param"))?;
        look.overrides.set_param(&self.param, self.value.clone()).map_err(|e| access::refusal(&e, "value"))?;
        access::check(&look, "value")?;
        let summary = json!({
            "character": self.character,
            "param": self.param,
            "value": self.value.clone().unwrap_or(Value::Null),
            "cleared": self.value.is_none(),
            "preset": look.preset_id(),
        });
        Ok(character::edit_plan(access::changes(ctx, &self.character, &look)?, summary))
    }
}

register_op!(LookSetParam);
