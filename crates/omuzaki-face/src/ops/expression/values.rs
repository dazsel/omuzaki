//! `face.expression.values`: previews one named expression at a weight. A query, so it changes nothing.

use omuzaki_anim_api::{CHANNELS, ChannelBuf};
use omuzaki_char_api::CharId;
use omuzaki_face_api::{BrowParams, ExpressionId, EyeEffects, EyeParams, MouthParams, PaintParams, narrow};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::store::missing;
use crate::ops_shared::edit::{effective, stored_face};

/// Previews a named expression at a weight: the channel values it blends to on a neutral bus (clamped to their ranges),
/// and the paint parameters the character's mapping makes of them. The output's `channels` lists only the non-zero
/// channels, so a channel it leaves out is 0. Check an expression with this before it is used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpressionValues {
    /// The character whose expression is previewed.
    pub character: CharId,
    /// The expression to preview.
    pub id: ExpressionId,
    /// How much of it applies, from `0` (off) to `1` (full strength), before its curve shapes it.
    pub weight: f64,
}

impl Op for ExpressionValues {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.values",
            summary: "Preview a named expression at a weight: its clamped channel values on a neutral bus and the paint \
                      parameters they give",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_weight(self.weight)?;
        let face = stored_face(ctx, &self.character)?;
        let profile = effective(face.as_ref())?;
        let expression = profile.expressions.0.get(&self.id).ok_or_else(|| missing(&self.id))?;
        let weight = narrow(self.weight);
        let buf =
            profile.expressions.blend(&ChannelBuf::NEUTRAL, &[(&self.id, weight)]).map_err(|_| missing(&self.id))?;
        let paint = profile.mapping.apply(&buf);
        Ok(Plan::answer(json!({
            "character": self.character.to_string(),
            "id": self.id.as_str(),
            "weight": self.weight,
            "applied": shortest(expression.curve.eval(weight)),
            "channels": channels(&buf),
            "paint": paint_summary(paint),
        })))
    }
}

register_op!(ExpressionValues);

/// Refuses a weight outside `0..=1`, naming `weight`.
fn check_weight(weight: f64) -> Result<(), OpError> {
    if weight.is_finite() && (0.0..=1.0).contains(&weight) {
        return Ok(());
    }
    Err(OpError::invalid("weight", format!("weight = {weight} is out of range: use a number from 0 to 1"))
        .hint("0 leaves the expression off and 1 applies it in full; its curve shapes the values in between"))
}

/// The channels the blend leaves non-zero, by name. The neutral bus is zero everywhere else, so the rest is left out.
fn channels(buf: &ChannelBuf) -> Value {
    Value::Object(
        CHANNELS
            .iter()
            .filter(|info| buf.get(info.channel) != 0.0)
            .map(|info| (info.name.to_owned(), shortest(buf.get(info.channel))))
            .collect(),
    )
}

/// The whole paint summary. `PaintParams` is not serialisable, so every field is named here. The destructuring has no
/// `..`, so a field added to `PaintParams` fails to compile until the summary lists it.
fn paint_summary(paint: PaintParams) -> Value {
    let PaintParams {
        left_eye,
        right_eye,
        eye_tilt,
        pupil,
        gaze,
        vergence,
        effects,
        left_brow,
        right_brow,
        brow_arch,
        mouth,
        blush,
        cheek_puff,
        sweat,
        anger_vein,
        nose_sneer,
    } = paint;
    json!({
        "left_eye": eye_summary(left_eye),
        "right_eye": eye_summary(right_eye),
        "left_brow": brow_summary(left_brow),
        "right_brow": brow_summary(right_brow),
        "brow_arch": shortest(brow_arch),
        "eye_tilt": shortest(eye_tilt),
        "pupil": shortest(pupil),
        "gaze": [shortest(gaze.x), shortest(gaze.y)],
        "vergence": shortest(vergence),
        "effects": effects_summary(effects),
        "mouth": mouth_summary(mouth),
        "blush": shortest(blush),
        "cheek_puff": shortest(cheek_puff),
        "sweat": shortest(sweat),
        "anger_vein": shortest(anger_vein),
        "nose_sneer": shortest(nose_sneer),
    })
}

/// One eye's parts, destructured without `..` for the same reason as [`paint_summary`].
fn eye_summary(eye: EyeParams) -> Value {
    let EyeParams { open, close, arc, lower_lid } = eye;
    fields(&[("open", open), ("close", close), ("arc", arc), ("lower_lid", lower_lid)])
}

/// One brow's parts, destructured without `..`.
fn brow_summary(brow: BrowParams) -> Value {
    let BrowParams { height, tilt } = brow;
    fields(&[("height", height), ("tilt", tilt)])
}

/// The eye effects, destructured without `..`.
fn effects_summary(effects: EyeEffects) -> Value {
    let EyeEffects { heart, star, gloom, shock, squeeze, spiral, sparkle, tears, blank, dot, stare, pill } = effects;
    fields(&[
        ("heart", heart),
        ("star", star),
        ("gloom", gloom),
        ("shock", shock),
        ("squeeze", squeeze),
        ("spiral", spiral),
        ("sparkle", sparkle),
        ("tears", tears),
        ("blank", blank),
        ("dot", dot),
        ("stare", stare),
        ("pill", pill),
    ])
}

/// The mouth's parts, destructured without `..`.
fn mouth_summary(mouth: MouthParams) -> Value {
    let MouthParams {
        open,
        wide,
        smile_left,
        smile_right,
        round,
        shift,
        press,
        pucker,
        teeth_upper,
        teeth_lower,
        tongue,
        fangs,
        teeth_sharp,
    } = mouth;
    fields(&[
        ("open", open),
        ("wide", wide),
        ("smile_left", smile_left),
        ("smile_right", smile_right),
        ("round", round),
        ("shift", shift),
        ("press", press),
        ("pucker", pucker),
        ("teeth_upper", teeth_upper),
        ("teeth_lower", teeth_lower),
        ("tongue", tongue),
        ("fangs", fangs),
        ("teeth_sharp", teeth_sharp),
    ])
}

/// A JSON object of named numbers.
fn fields(pairs: &[(&str, f32)]) -> Value {
    Value::Object(pairs.iter().map(|(name, v)| ((*name).to_owned(), shortest(*v))).collect::<Map<String, Value>>())
}

/// An `f32` as the shortest decimal that reads back to it (`0.36`, not `0.3600000143`), so the output reads cleanly and
/// the same value always prints the same way.
fn shortest(v: f32) -> Value {
    Value::from(v.to_string().parse::<f64>().unwrap_or(f64::from(v)))
}
