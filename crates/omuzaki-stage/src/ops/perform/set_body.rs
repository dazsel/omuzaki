//! `perform.set_body`: the living body's dials (PF-24, AP-05): how much the chest follows the head, the weight shifts
//! and sway, the breath, the posture the face's emotion gives, the co-speech gestures, the arms' follow-through and
//! the idle fidgets, over a preset (natural, energetic, calm, shy, sleepy). `body.styles` lists the presets and
//! `measure.body_motion` measures a style before it goes live.

use omuzaki_anim_api::{BodyStyle, Handedness, MAX_BODY_AMOUNT, Personality};
use omuzaki_ops_api::{Effect, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_stage_api::ids::MemberId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::op_error;
use crate::ops::act::performing;

/// Change how a performing character's body moves on its own; fields left out stay as they are (a `preset` given
/// alone resets every amount to 1 on it). Amounts are `0` (off) to `2` (double), `1` as the preset has it. Queued for
/// the performer's next step; the step records the call that puts the previous style back.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetBody {
    /// The cast member; the scene's subject when left out.
    #[serde(default)]
    pub member: Option<MemberId>,
    /// The temperament: natural, energetic, calm, shy or sleepy.
    #[serde(default)]
    pub preset: Option<Personality>,
    /// Master amount over everything.
    #[serde(default)]
    pub amount: Option<f32>,
    /// Weight shifts and sway.
    #[serde(default)]
    pub sway: Option<f32>,
    /// How much the chest and shoulders turn with the head.
    #[serde(default)]
    pub follow: Option<f32>,
    /// Breath in the chest and shoulders.
    #[serde(default)]
    pub breath: Option<f32>,
    /// Posture from the face's emotion, and the laugh's shake.
    #[serde(default)]
    pub emotion: Option<f32>,
    /// Co-speech gestures: how often and how big.
    #[serde(default)]
    pub gestures: Option<f32>,
    /// The arms' follow-through.
    #[serde(default)]
    pub arms: Option<f32>,
    /// Idle fidgets.
    #[serde(default)]
    pub fidgets: Option<f32>,
    /// The leading hand.
    #[serde(default)]
    pub handedness: Option<Handedness>,
}

impl SetBody {
    /// `current` with this change applied, or the field that is out of range.
    fn apply(&self, current: BodyStyle) -> Result<BodyStyle, OpError> {
        let mut style = match self.preset {
            Some(preset) if self.is_preset_only() => BodyStyle { handedness: current.handedness, ..BodyStyle::preset(preset) },
            Some(preset) => BodyStyle { preset, ..current },
            None => current,
        };
        let fields: [(&str, Option<f32>, &mut f32); 8] = [
            ("amount", self.amount, &mut style.amount),
            ("sway", self.sway, &mut style.sway),
            ("follow", self.follow, &mut style.follow),
            ("breath", self.breath, &mut style.breath),
            ("emotion", self.emotion, &mut style.emotion),
            ("gestures", self.gestures, &mut style.gestures),
            ("arms", self.arms, &mut style.arms),
            ("fidgets", self.fidgets, &mut style.fidgets),
        ];
        for (name, value, slot) in fields {
            if let Some(v) = value {
                if !(0.0..=MAX_BODY_AMOUNT).contains(&v) {
                    return Err(OpError::invalid(name, format!("must be 0 to {MAX_BODY_AMOUNT}, got {v}")));
                }
                *slot = v;
            }
        }
        if let Some(hand) = self.handedness {
            style.handedness = hand;
        }
        Ok(style)
    }

    fn is_preset_only(&self) -> bool {
        let Self { member: _, preset: _, amount, sway, follow, breath, emotion, gestures, arms, fidgets, handedness: _ } =
            self;
        [amount, sway, follow, breath, emotion, gestures, arms, fidgets].iter().all(|v| v.is_none())
    }

    /// The call that sets every dial to `style`.
    fn all(member: Option<MemberId>, style: &BodyStyle) -> Self {
        Self {
            member,
            preset: Some(style.preset),
            amount: Some(style.amount),
            sway: Some(style.sway),
            follow: Some(style.follow),
            breath: Some(style.breath),
            emotion: Some(style.emotion),
            gestures: Some(style.gestures),
            arms: Some(style.arms),
            fidgets: Some(style.fidgets),
            handedness: Some(style.handedness),
        }
    }

    fn call(&self) -> Result<OpCall, OpError> {
        let args = serde_json::to_value(self).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(OpCall { op: Self::meta().name.to_owned(), args })
    }
}

impl Op for SetBody {
    fn meta() -> OpMeta {
        OpMeta {
            name: "perform.set_body",
            summary: "Set how a performing character's body moves on its own: preset (natural, energetic, calm, shy, sleepy) and the sway, follow, breath, emotion, gestures, arms and fidgets amounts (0 to 2).",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let performing = performing(ctx, self.member.as_ref())?;
        let host = performing.host();
        let before = host.body().unwrap_or_default();
        let after = self.apply(before)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "style": after })));
        }
        host.set_body(after).map_err(op_error)?;
        let member = performing.member().cloned();
        let effect = Effect {
            summary: "changed how the body moves".to_owned(),
            undo: Self::all(member.clone(), &before).call()?,
            redo: Self::all(member, &after).call()?,
        };
        Ok(Plan { effect: Some(effect), ..Plan::answer(json!({ "style": after })) })
    }
}

omuzaki_ops_api::register_op!(SetBody);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_keeps_what_it_leaves_out_and_refuses_out_of_range() {
        let current = BodyStyle { gestures: 0.5, ..BodyStyle::preset(Personality::Calm) };
        let change = SetBody { sway: Some(1.5), ..SetBody::default() };
        let style = change.apply(current).expect("in range");
        assert_eq!((style.preset, style.gestures, style.sway), (Personality::Calm, 0.5, 1.5));
        let preset_only = SetBody { preset: Some(Personality::Energetic), ..SetBody::default() };
        assert_eq!(preset_only.apply(current).expect("ok"), BodyStyle::preset(Personality::Energetic));
        let preset_and = SetBody { preset: Some(Personality::Shy), arms: Some(0.0), ..SetBody::default() };
        let style = preset_and.apply(current).expect("ok");
        assert_eq!((style.preset, style.arms, style.gestures), (Personality::Shy, 0.0, 0.5));
        assert!(SetBody { gestures: Some(3.0), ..SetBody::default() }.apply(current).is_err());
        assert!(SetBody { amount: Some(f32::NAN), ..SetBody::default() }.apply(current).is_err());
    }
}
