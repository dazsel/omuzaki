//! `act.react`: a physical reaction on a performing body (master checklist §6; #2581): a bonk on the head, a shove
//! on a shoulder, a fall. The living body knocks an active ragdoll of the spine, head and arms with it and is back on
//! the performed pose (tracking included) after `recover_s`, exactly; the face keeps its expression throughout. Like
//! the other acts it is a live action: logged, scope `perform`, nothing to undo (the reaction ends by itself).
//! `measure.reaction` measures one before it goes live.

use omuzaki_anim_api::{BodyPart, Impact, ImpactKind, MAX_IMPACT_STRENGTH, MAX_RECOVER_S, MIN_RECOVER_S};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_stage_api::ids::MemberId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{live, performing, within};
use crate::ops::perform::op_error;

/// Knock a performing character's body: a `hit` rocks the part it lands on and springs back, a `fall` makes the
/// body give way the way it was pushed and pull itself back up. The face keeps its expression and the head is back
/// where tracking puts it after `recover_s`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "at": "head", "from": "above" },
    { "at": "face", "strength": 1.5 },
    { "kind": "fall", "at": "chest", "from": "front", "recover_s": 3 },
    { "at": "left_shoulder", "travel": [0.0, 0.0, -1.0], "strength": 2 }
]))]
pub struct React {
    /// The cast member; the scene's subject when left out.
    #[serde(default)]
    pub member: Option<MemberId>,
    /// `hit` (default) or `fall`.
    #[serde(default)]
    pub kind: ImpactKind,
    /// Where it lands: `head` (default), `face`, `chest`, `stomach`, `left_shoulder` or `right_shoulder`.
    #[serde(default)]
    pub at: BodyPart,
    /// Where the blow comes from, from the character's point of view: front (default), behind, above, left or right.
    /// `travel` overrides it.
    #[serde(default)]
    pub from: Option<BlowFrom>,
    /// The way the blow travels in the character's space (`+X` its left, `+Y` up, `+Z` the way it faces).
    #[serde(default)]
    pub travel: Option<[f32; 3]>,
    /// How hard, 0 to 3; 1 rocks the head about 15 degrees. Default 1.
    #[serde(default)]
    pub strength: Option<f32>,
    /// Seconds until the body is back on its performed pose, 0.3 to 10. Default 1.2 for a hit, 3 for a fall.
    #[serde(default)]
    pub recover_s: Option<f32>,
}

/// Where a blow comes from, as the character sees it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlowFrom {
    /// In front: pushes the body back.
    Front,
    /// Behind: pushes it forward.
    Behind,
    /// Above: comes down on it.
    Above,
    /// The character's left: pushes it to its right.
    Left,
    /// The character's right: pushes it to its left.
    Right,
}

impl BlowFrom {
    /// The way a blow from here travels.
    const fn travel(self) -> [f32; 3] {
        match self {
            Self::Front => [0.0, 0.0, -1.0],
            Self::Behind => [0.0, 0.0, 1.0],
            Self::Above => [0.0, -1.0, 0.0],
            Self::Left => [-1.0, 0.0, 0.0],
            Self::Right => [1.0, 0.0, 0.0],
        }
    }
}

impl React {
    /// The impact this call plays, or the field that is out of range.
    fn impact(&self) -> Result<Impact, OpError> {
        let travel = match (self.travel, self.from) {
            (Some(t), _) => {
                if !t.iter().all(|v| v.is_finite()) || t.iter().all(|v| *v == 0.0) {
                    return Err(OpError::invalid("travel", "must be three finite numbers, not all zero"));
                }
                t
            }
            (None, Some(from)) => from.travel(),
            (None, None) => BlowFrom::Front.travel(),
        };
        let strength = within("strength", self.strength.unwrap_or(1.0), 0.0, MAX_IMPACT_STRENGTH)?;
        let base = match self.kind {
            ImpactKind::Hit => Impact::hit(self.at, travel, strength),
            ImpactKind::Fall => Impact::fall(self.at, travel, strength),
        };
        let recover_s = within("recover_s", self.recover_s.unwrap_or(base.recover_s), MIN_RECOVER_S, MAX_RECOVER_S)?;
        Ok(base.recovering_in(recover_s).clamped())
    }
}

impl Op for React {
    fn meta() -> OpMeta {
        live(
            "act.react",
            "Knock a performing character's body, live: a hit on the head, face, chest, stomach or a shoulder that rocks it and springs back, or a fall that gives way and gets back up; back on the performed pose after recover_s.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let impact = self.impact()?;
        let performing = performing(ctx, self.member.as_ref())?;
        let member = performing.member().map(MemberId::as_str);
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "member": member, "impact": impact })));
        }
        performing.host().react(impact).map_err(op_error)?;
        Ok(Plan::answer(json!({ "member": member, "impact": impact, "back_in_s": impact.recover_s })))
    }
}

register_op!(React);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_becomes_its_impact_and_refuses_what_is_out_of_range() {
        let bonk = React { at: BodyPart::Head, from: Some(BlowFrom::Above), ..React::default() };
        let impact = bonk.impact().expect("in range");
        assert_eq!((impact.kind, impact.at, impact.travel), (ImpactKind::Hit, BodyPart::Head, [0.0, -1.0, 0.0]));
        assert!((impact.recover_s - 1.2).abs() < 1e-6);
        let fall = React { kind: ImpactKind::Fall, travel: Some([0.0, 0.0, 2.0]), ..React::default() };
        let impact = fall.impact().expect("in range");
        assert!((impact.recover_s - 3.0).abs() < 1e-6 && (impact.travel[2] - 1.0).abs() < 1e-6);
        assert!(React { strength: Some(4.0), ..React::default() }.impact().is_err());
        assert!(React { recover_s: Some(0.1), ..React::default() }.impact().is_err());
        assert!(React { travel: Some([0.0; 3]), ..React::default() }.impact().is_err());
    }
}
