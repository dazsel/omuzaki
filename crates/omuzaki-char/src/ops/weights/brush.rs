//! `rig.weights.brush`: paint a weight correction onto a part.

use omuzaki_char_api::{BrushFalloff, CharId, JointRef, PartId, WeightBrush};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{brushes_of, build_character, check_brush, check_joint, store};
use crate::ops::character::access;

/// Adds a weight brush to a part: within `radius` of `centre`, `strength` of each vertex's skin weight moves to `joint`.
///
/// Use it where the generator's skin is wrong: a shoulder that folds, a skirt hem that clings to the hips. The brush is
/// stored with the character and applied each time the part builds, so it survives later edits to the part's
/// parameters. `centre` and `radius` are bind-space metres (the character as sculpted: +x is its left, y is up). The
/// joint must be a bone of the character's skeleton: a name it lacks is refused with the nearest bone's name. A brush
/// that reaches no vertex is still kept, and `rig.weights.report` lists it. The falloff is `smooth` unless given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "body", "centre": [0.2, 1.3, 0.0], "radius": 0.15, "joint": {"humanoid": "leftLowerArm"}, "strength": 0.6}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct WeightsBrush {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part whose skin the brush corrects. Must exist.
    pub part: PartId,
    /// The centre, bind space, metres, as `[x, y, z]`; each from -10 to 10.
    pub centre: [f64; 3],
    /// The radius, metres; must be above 0.
    pub radius: f64,
    /// The joint that gains weight: `{"humanoid": "lowerArm"}` for a humanoid bone, `{"named": "<bone>"}` for an extra one.
    pub joint: JointRef,
    /// The share of a vertex's weight that moves to `joint` at the centre; from 0 to 1.
    pub strength: f64,
    /// How the strength fades toward the edge: `smooth` (the default), `linear` or `constant`.
    #[serde(default)]
    pub falloff: BrushFalloff,
}

impl Op for WeightsBrush {
    fn meta() -> OpMeta {
        OpMeta {
            name: "rig.weights.brush",
            summary: "Paint a weight brush on a part: move skin weight toward a joint within a radius.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        let brush = WeightBrush {
            centre: self.centre,
            radius: self.radius,
            joint: self.joint.clone(),
            strength: self.strength,
            falloff: self.falloff,
        };
        check_brush(&brush)?;
        let built = build_character(&self.character, &def)?;
        check_joint(&built, &self.character, &self.joint)?;
        let mut brushes = brushes_of(&def, &self.part);
        brushes.push(brush);
        let changes = store(ctx, &self.character, &self.part, &brushes)?;
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "index": brushes.len() - 1,
            "brushes": brushes.len(),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(WeightsBrush);
