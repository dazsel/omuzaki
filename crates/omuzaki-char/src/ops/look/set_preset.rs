//! `look.set_preset`: draws a character in a look preset.

use omuzaki_char_api::{CharId, LookDef, LookParams, LookPresetId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use crate::ops::character::access as character;

/// Draws a character in a look preset (LK-06: `soft-sd`, `cel-flat`, `painterly`, `figure`, `pixel`). The overrides
/// made for the previous preset are cleared unless `keep_overrides` is set, so the character looks exactly like the
/// preset; one undo restores the previous look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "preset": "soft-sd"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct LookSetPreset {
    /// The character to restyle.
    pub character: CharId,
    /// The preset's id; `look.list_presets` lists them.
    pub preset: LookPresetId,
    /// Keep the person's overrides on top of the new preset instead of clearing them.
    #[serde(default)]
    pub keep_overrides: bool,
}

impl Op for LookSetPreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.set_preset",
            summary: "Draw a character in a look preset (soft-sd, cel-flat, painterly, figure, pixel), clearing or keeping \
                      the look's overrides.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = access::load(ctx, &self.character)?;
        let overrides = if self.keep_overrides { before.overrides.clone() } else { LookParams::default() };
        let cleared = !self.keep_overrides && !before.overrides.is_empty();
        let after = LookDef { preset: Some(self.preset.clone()), overrides };
        access::check(&after, "preset")?;
        let summary = json!({
            "character": self.character,
            "preset": self.preset,
            "previous": before.preset_id(),
            "cleared_overrides": cleared,
        });
        Ok(character::edit_plan(access::changes(ctx, &self.character, &after)?, summary))
    }
}

register_op!(LookSetPreset);
