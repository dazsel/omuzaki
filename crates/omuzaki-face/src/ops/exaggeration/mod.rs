//! `face.set_exaggeration` (FE-06): turns a face's exaggeration dial, from realistic through the anime defaults to a
//! chibi extreme, and sets the subtle-performance expander, in one undoable edit. The dial and its presets are in
//! [`crate::exaggeration`].

use omuzaki_char_api::CharId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::exaggeration::{Preset, gains_object};
use crate::ops::mapping::plan_edit;

/// The subtle-performance expander: below `knee`, each tracked (`ARKit`) channel is multiplied by `gain`, so small
/// movements read; above it the response runs straight to full.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AmplifySubtle {
    /// Where the boost stops, as a channel value, `0..=0.5`. 0 turns the expander off. Default 0.2.
    #[serde(default = "default_knee")]
    pub knee: f64,
    /// The boost below the knee, `1..=4`, with `knee x gain` at most 1. Default 2.
    #[serde(default = "default_gain")]
    pub gain: f64,
}

fn default_knee() -> f64 {
    0.2
}

fn default_gain() -> f64 {
    2.0
}

/// Sets how exaggerated a character's face reads: a preset (`realistic`, `anime`, `chibi-extreme`) or a dial `amount`
/// from -1 (realistic) through 0 (the anime defaults) to +1 (chibi extreme), and optionally the subtle-movement
/// expander. The dial rewrites the face's gain coefficients from the defaults, so turning it never compounds and amount
/// 0 restores the defaults; the resting face and thresholds are left alone. Per-channel response curves for one
/// performer are retarget calibration, not this op.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "kit", "preset": "chibi-extreme" }, { "character": "kit", "amount": -0.5, "amplify_subtle": { "knee": 0.2, "gain": 2.0 } }],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetExaggeration {
    /// The character whose face changes.
    pub character: CharId,
    /// A named point on the dial. Give this or `amount`, not both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    /// The dial, `-1..=1`: negative toward realistic, positive toward chibi extreme, 0 the anime defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    /// The subtle-movement expander; `{ "knee": 0 }` turns it off. Left out, the expander is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amplify_subtle: Option<AmplifySubtle>,
}

impl Op for SetExaggeration {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_exaggeration",
            summary: "Set how exaggerated a face reads (a preset or a dial from realistic through anime to chibi \
                      extreme) and the subtle-movement expander, validate the whole face, and return the changed paths",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let amount = match (self.preset, self.amount) {
            (Some(_), Some(_)) => {
                return Err(OpError::invalid("amount", "give a preset or an amount, not both")
                    .hint("presets are amounts: realistic -1, anime 0, chibi-extreme 1"));
            }
            (Some(preset), None) => Some(preset.amount()),
            (None, Some(amount)) if !(amount.is_finite() && (-1.0..=1.0).contains(&amount)) => {
                return Err(OpError::invalid("amount", format!("{amount} is out of range: use a number from -1 to 1"))
                    .hint("-1 is realistic, 0 the anime defaults, 1 chibi extreme"));
            }
            (None, amount) => amount,
        };
        if amount.is_none() && self.amplify_subtle.is_none() {
            return Err(OpError::invalid("preset", "nothing to set")
                .hint("give a preset, an amount, or amplify_subtle"));
        }
        let mut mapping = match amount {
            Some(amount) => gains_object(amount).map_err(OpError::internal)?,
            None => Value::Object(serde_json::Map::new()),
        };
        if let (Some(amp), Value::Object(map)) = (&self.amplify_subtle, &mut mapping) {
            map.insert("amplify".to_owned(), json!({ "knee": amp.knee, "gain": amp.gain }));
        }
        let mut plan = plan_edit(ctx, &self.character, &["mapping".to_owned()], mapping)?;
        if let (Some(Value::Object(output)), Some(amount)) = (&mut plan.output, amount) {
            output.insert("amount".to_owned(), json!(amount));
        }
        Ok(plan)
    }
}

register_op!(SetExaggeration);
