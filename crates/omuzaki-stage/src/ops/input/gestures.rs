//! `input.gestures`: the face gestures that press trigger keys (PF-25), what each is bound to and which show now.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_track::gesture::Gesture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::host;
use crate::perform::host::PerformHost;

/// List the face gestures that can show an expression: a tongue out, a wink with either eye, puffed cheeks and a wide
/// open mouth. Each presses its trigger key (for example `gesture:tongue_out`) while the performer holds it; bind an
/// expression to that key with `trigger.bind` to use it. The tongue needs a phone tracker (the webcam's model has no
/// tongue). The answer gives each gesture's key, the expression bound to it, if any, and whether it shows now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct Gestures {}

impl Op for Gestures {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.gestures",
            summary: "List the face gestures (tongue out, winks, puffed cheeks, wide mouth), their trigger keys and binds.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let active = host(ctx)?.gestures();
        let triggers = ctx.service::<dyn PerformHost>().map(PerformHost::triggers).unwrap_or_default();
        let rows: Vec<_> = Gesture::ALL
            .into_iter()
            .map(|gesture| {
                let bound = triggers.iter().find(|t| t.binding.key.eq_ignore_ascii_case(gesture.key()));
                json!({
                    "gesture": gesture,
                    "key": gesture.key(),
                    "expression": bound.map(|t| t.binding.expression.clone()),
                    "active": active.contains(&gesture),
                })
            })
            .collect();
        Ok(Plan::answer(json!({ "gestures": rows })))
    }
}

omuzaki_ops_api::register_op!(Gestures);
