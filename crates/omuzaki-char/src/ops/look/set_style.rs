//! `look.set_style`: moves a character, or one class of its surfaces, along the style axis (#2607).

use omuzaki_char_api::{CharId, MaterialClass};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::access;
use crate::ops::character::access as character;

/// Sets where a character sits on the style axis, from `0` (physical: subsurface skin, hair lit as fibres, metal as a
/// conductor, cloth with sheen) to `1` (the cel look the parts build), for the whole character or one class of surface
/// (`skin`, `hair`, `cloth`, `metal`, `eyes`), so a photoreal face can wear anime hair. Every value between is a
/// continuous blend with no step; lines thin out toward `0`. Leave `style` out to clear the setting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "style": 0.0},
        {"character": "kit", "style": 0.5, "class": "skin"},
        {"character": "kit", "class": "hair"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct LookSetStyle {
    /// The character.
    pub character: CharId,
    /// `0` physical to `1` cel. Left out: clear it (the preset's, or the cel look).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub style: Option<f32>,
    /// Only this class of surface. Left out: the whole character (`all`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<MaterialClass>,
}

impl Op for LookSetStyle {
    fn meta() -> OpMeta {
        OpMeta {
            name: "look.set_style",
            summary: "Set where a character (or its skin, hair, cloth, metal or eyes) sits on the style axis, 0 physical to 1 cel.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if let Some(style) = self.style
            && !(0.0..=1.0).contains(&style)
        {
            return Err(OpError::invalid("style", format!("{style} is outside 0 (physical) to 1 (cel)"))
                .hint("give a style from 0 to 1, or leave it out to clear it"));
        }
        let mut look = access::load(ctx, &self.character)?;
        let section = self.class.map_or("all", MaterialClass::as_str);
        let param = format!("{section}.style");
        look.overrides
            .set_param(&param, self.style.map(|s| json!(s)))
            .map_err(|e| access::refusal(&e, "style"))?;
        access::check(&look, "style")?;
        let summary = json!({ "character": self.character, "section": section, "style": self.style });
        Ok(character::edit_plan(access::changes(ctx, &self.character, &look)?, summary))
    }
}

register_op!(LookSetStyle);
