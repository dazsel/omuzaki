//! `body.set_hand_style`: how the character's hands are built (CB-02).

use omuzaki_char_api::{CharId, HandStyle};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::params;
use crate::ops::character::access;

/// The hand styles, as the op takes them.
const STYLES: [&str; 3] = ["mitten", "simple", "five_finger"];

/// Sets how a character's hands are built. The style chooses what the mesh shows; the hand's shape stays in its own
/// parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetHandStyle {
    /// The character whose hands change.
    pub character: CharId,
    /// The style: `mitten` (soft mittens with a thumb), `simple` (fused fingers) or `five_finger` (five fingers).
    pub style: String,
}

/// The hand style called `text`.
///
/// # Errors
/// `invalid` naming `style`, with the closest style and the full list when the name is not one.
fn parse_style(text: &str) -> Result<HandStyle, OpError> {
    serde_json::from_value(Value::String(text.to_owned()))
        .map_err(|_| params::unknown_name("style", "hand style", text, &STYLES))
}

impl Op for SetHandStyle {
    fn meta() -> OpMeta {
        OpMeta {
            name: "body.set_hand_style",
            summary: "Set how the character's hands are built: mitten, simple or five_finger",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let style = parse_style(&self.style)?;
        let def = access::load(ctx, &self.character)?;
        let body = params::load(&self.character, &def)?;
        let mut after = body.params.clone();
        let previous = body.params.hands;
        after.hands = style;

        let changes = params::changes(ctx, &body.params_path, &body.params, &after)?;
        let touched = params::changed_paths(&changes);
        Ok(access::edit_plan(
            changes,
            json!({
                "character": self.character,
                "style": self.style,
                "previous": previous,
                "changed": touched,
            }),
        ))
    }
}

register_op!(SetHandStyle);
