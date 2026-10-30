//! `input.gestures`: the face and hand gestures that press trigger keys (PF-25, PF-06), what each is bound to and which
//! show now.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_track::gesture::Gesture;
use omuzaki_track_api::body::HandGesture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::host;
use crate::perform::host::PerformHost;

/// List the gestures that can show an expression. Face: a tongue out, a wink with either eye, puffed cheeks and a wide
/// open mouth. Hands, while body tracking runs (`input.track_body`): an open palm, a fist, pointing, a peace sign and a
/// thumbs up, from either hand. Each presses its trigger key (for example `gesture:tongue_out`, `gesture:peace`) while
/// the performer holds it; bind an expression to that key with `trigger.bind` to use it. The tongue needs a phone
/// tracker (the webcam's model has no tongue). The answer gives each gesture's key, the expression bound to it, if
/// any, and whether it shows now; hand gestures are listed under `hands`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct Gestures {}

impl Op for Gestures {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.gestures",
            summary: "List the face and hand gestures (tongue out, winks, peace sign, fist...), their trigger keys and binds.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let input = host(ctx)?;
        let (active, hands_active) = (input.gestures(), input.hand_gestures());
        let triggers = ctx.service::<dyn PerformHost>().map(PerformHost::triggers).unwrap_or_default();
        let bound = |key: &str| {
            triggers.iter().find(|t| t.binding.key.eq_ignore_ascii_case(key)).map(|t| t.binding.expression.clone())
        };
        let rows: Vec<_> = Gesture::ALL
            .into_iter()
            .map(|gesture| {
                json!({
                    "gesture": gesture,
                    "key": gesture.key(),
                    "expression": bound(gesture.key()),
                    "active": active.contains(&gesture),
                })
            })
            .collect();
        let hands: Vec<_> = HandGesture::ALL
            .into_iter()
            .map(|gesture| {
                json!({
                    "gesture": gesture,
                    "key": gesture.key(),
                    "expression": bound(gesture.key()),
                    "active": hands_active.contains(&gesture),
                })
            })
            .collect();
        Ok(Plan::answer(json!({ "gestures": rows, "hands": hands })))
    }
}

omuzaki_ops_api::register_op!(Gestures);
