//! `input.track_body`: track the arms and hands beside the face (PF-06 hands, PF-07 arms).

use omuzaki_ops_api::{Effect, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_track_api::body::BodyConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{host, mode, op_error, persist, to_value};
use crate::perform::input::InputBody;

/// Track the person's arms and hands from the camera beside the face, so the character's shoulders, elbows, wrists
/// and every finger follow them; or stop with every switch off. `arms` tracks the chest's turn and both arms, `hands`
/// each wrist's turn and all ten fingers, `precise` uses the heavier pose model (steadier elbows and wrists, about
/// twice the cost). A missing field is on for `arms` and `hands` and off for `precise`. It follows the camera: it runs
/// while the capture does (`input.preview`), and a part the camera loses eases back to the animation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [
    { "arms": true, "hands": true },
    { "arms": true, "hands": false, "precise": true },
    { "arms": false, "hands": false }
]))]
pub struct TrackBody {
    /// Track the chest's turn, the shoulders, elbows and wrists. Default on.
    #[serde(default)]
    pub arms: Option<bool>,
    /// Track each wrist's turn and every finger. Default on.
    #[serde(default)]
    pub hands: Option<bool>,
    /// Use the heavier pose model. Default off.
    #[serde(default)]
    pub precise: Option<bool>,
}

impl TrackBody {
    fn config(&self) -> BodyConfig {
        let on = BodyConfig::default();
        BodyConfig {
            arms: self.arms.unwrap_or(on.arms),
            hands: self.hands.unwrap_or(on.hands),
            precise: self.precise.unwrap_or(on.precise),
        }
    }
}

/// What a body config reads as, for the history.
fn describe(config: BodyConfig) -> String {
    let parts = match (config.arms, config.hands) {
        (true, true) => "arms and hands",
        (true, false) => "arms",
        (false, true) => "hands",
        (false, false) => return "body tracking off".to_owned(),
    };
    let model = if config.precise { ", precise" } else { "" };
    format!("tracking the {parts}{model}")
}

impl Op for TrackBody {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.track_body",
            summary: "Track the arms and hands from the camera beside the face (arms, hands, precise), or stop.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let config = self.config();
        // Stopping only reads less of the person, so anyone may; reading more of them is the person's to confirm.
        if config.any() {
            ctx.require_person("tracking the arms and hands reads more of the person from the camera")?;
        }
        let before = host(ctx)?.track_body(config, mode(ctx)).map_err(op_error)?;
        let saved = persist::<InputBody>(ctx, &config)?;
        let answer = json!({
            "body": to_value(config)?,
            "before": to_value(before)?,
            "saved": saved,
            "dry_run": ctx.dry_run,
        });
        let call = |c: BodyConfig| -> Result<OpCall, OpError> {
            Ok(OpCall { op: Self::meta().name.to_owned(), args: to_value(c)? })
        };
        let effect = (before != config)
            .then(|| -> Result<Effect, OpError> {
                Ok(Effect { summary: describe(config), undo: call(before)?, redo: call(config)? })
            })
            .transpose()?;
        Ok(Plan { effect, ..Plan::answer(answer) })
    }
}

omuzaki_ops_api::register_op!(TrackBody);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_track_arms_and_hands_on_the_light_model() {
        let all: TrackBody = serde_json::from_value(json!({})).unwrap_or_default();
        assert_eq!(all.config(), BodyConfig { arms: true, hands: true, precise: false });
        let off: TrackBody = serde_json::from_value(json!({ "arms": false, "hands": false })).unwrap_or_default();
        assert!(!off.config().any());
        assert_eq!(describe(off.config()), "body tracking off");
        assert_eq!(describe(BodyConfig { arms: true, hands: false, precise: true }), "tracking the arms, precise");
    }
}
