//! `face.describe_mapping`: every coefficient of a face's mapping, with its default, its current value and its formula, so
//! an agent sees what each number does before it sets one.

use std::collections::BTreeMap;

use omuzaki_char_api::CharId;
use omuzaki_face_api::FaceMapping;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ops_shared::edit::{effective, stored_face, to_json};

/// Lists every mapping coefficient of a character's face: its path, default, current value and formula.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeMapping {
    /// The character whose mapping to describe.
    pub character: CharId,
}

impl Op for DescribeMapping {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.describe_mapping",
            summary: "List every exaggeration coefficient of a face's mapping, with its default, current value and formula",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let stored = stored_face(ctx, &self.character)?;
        let current = leaves(&to_json(&effective(stored.as_ref())?.mapping)?);
        let mut coefficients = Vec::new();
        for (path, default) in leaves(&to_json(&FaceMapping::default())?) {
            let formula = formula(&path).ok_or_else(|| OpError::internal(format!("no formula for {path}")))?;
            let now = current.get(&path).ok_or_else(|| OpError::internal(format!("no current value for {path}")))?;
            coefficients.push(json!({
                "path": format!("face/mapping/{path}"),
                "default": default,
                "current": now,
                "formula": formula,
            }));
        }
        Ok(Plan::answer(json!({ "character": self.character.to_string(), "coefficients": coefficients })))
    }
}

register_op!(DescribeMapping);

/// Every leaf of a JSON object as `section/key` paths, in order.
fn leaves(value: &Value) -> BTreeMap<String, Value> {
    fn walk(value: &Value, prefix: &str, into: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}/{key}") };
                    walk(child, &path, into);
                }
            }
            leaf => {
                into.insert(prefix.to_owned(), leaf.clone());
            }
        }
    }
    let mut into = BTreeMap::new();
    walk(value, "", &mut into);
    into
}

/// The formula of the coefficient at `path` (`eyes/wide`), or `None` when the table has no such entry.
fn formula(path: &str) -> Option<&'static str> {
    FORMULAS.iter().find(|(p, _)| *p == path).map(|(_, f)| *f)
}

/// Every coefficient and its formula, as the design doc's table states them (`docs/design/char-face-api.md`). Channels are
/// named as on the bus. A test holds this list to the coefficients `FaceMapping` has, so none is missing or stale.
const FORMULAS: &[(&str, &str)] = &[
    (
        "rest/eye_open",
        "base openness of both eyes (1 is the rest aperture): open = this + eyes.wide x eyeWide - eyes.squint x eyeSquint + eyes.stare_open x eyesStare, clamped to 0..1.9",
    ),
    (
        "rest/lower_lid",
        "base lower-lid raise: lid = this + eyes.squint_lid x eyeSquint + eyes.cheek_lid x cheekSquint, clamped to 0..1",
    ),
    ("rest/eye_tilt", "base lid tilt: tilt = this + eyes.tilt_from_brows x mean brow tilt, clamped to -2..2"),
    ("rest/pupil", "base pupil size: pupil = this x (1 - eyes.shock_pupil x eyesShock), clamped to 0.4..1.6"),
    (
        "rest/brow_height",
        "base brow height: height = this + brows.inner_up x browInnerUp + brows.outer_up x browOuterUp - brows.down x browDown - brows.sneer x noseSneer, clamped to -1.5..1.5",
    ),
    (
        "rest/brow_tilt",
        "base brow angle: tilt = this - brows.inner_up_tilt x browInnerUp + brows.outer_up_tilt x browOuterUp + brows.down_tilt x browDown, clamped to -2.5..2.5",
    ),
    (
        "rest/brow_arch",
        "base brow arch: arch = this + brows.outer_up_arch x browOuterUp - brows.inner_up_arch x browInnerUp, clamped to -1..1",
    ),
    ("rest/mouth_smile", "base mouth-corner lift: smile = this + mouth.smile x corner lift, clamped to -2..2"),
    (
        "rest/mouth_wide",
        "base mouth width: wide = this + mouth.stretch x mouthStretch + mouth.wide_smile x mean corner lift - mouth.wide_pucker x mouthPucker, clamped to 0.3..2",
    ),
    (
        "rest/blush",
        "base blush: blush = this + cheeks.puff_blush x cheekPuff + cheeks.squint_blush x cheekSquint + cheeks.blush x blush, clamped to 0..1.6",
    ),
    ("eyes/wide", "openness added to both eyes per unit of eyeWide"),
    ("eyes/squint", "openness taken from both eyes per unit of eyeSquint"),
    ("eyes/squint_lid", "lower lid raised per unit of eyeSquint"),
    ("eyes/cheek_lid", "lower lid raised per unit of cheekSquint"),
    ("eyes/tilt_from_brows", "how much the mean brow angle tilts the lids (0 turns it off)"),
    ("eyes/shock_pupil", "how far eyesShock shrinks the pupil, 0..1"),
    ("eyes/stare_open", "openness added per unit of eyesStare (a stare holds the lids wide)"),
    ("brows/inner_up", "brow height added per unit of browInnerUp"),
    ("brows/outer_up", "brow height added per unit of browOuterUp"),
    ("brows/down", "brow height lost per unit of browDown"),
    ("brows/sneer", "brow height lost per unit of noseSneer"),
    ("brows/inner_up_tilt", "brow angle lost per unit of browInnerUp (inner brows slant up)"),
    ("brows/outer_up_tilt", "brow angle added per unit of browOuterUp"),
    ("brows/down_tilt", "brow angle added per unit of browDown"),
    ("brows/outer_up_arch", "brow arch added per unit of browOuterUp"),
    ("brows/inner_up_arch", "brow arch lost per unit of browInnerUp"),
    (
        "mouth/open",
        "jaw opening per unit of jawOpen: open = this x max(jaw, lips parted), jaw = max(0, jawOpen - close_cancel x \
         mouthClose), clamped to 0..1.5",
    ),
    ("mouth/close_cancel", "how much mouthClose cancels from jawOpen"),
    ("mouth/part_funnel", "jaw-equivalent opening per unit of mouthFunnel when the jaw is closed (lips parted)"),
    ("mouth/part_upper_up", "jaw-equivalent opening per unit of the mean mouthUpperUp* when the jaw is closed"),
    ("mouth/part_lower_down", "jaw-equivalent opening per unit of the mean mouthLowerDown* when the jaw is closed"),
    ("mouth/part_tongue", "jaw-equivalent opening per unit of tongueOut when the jaw is closed"),
    ("mouth/smile", "mouth-corner lift per unit of mouthSmile*"),
    ("mouth/frown", "mouth-corner lift lost per unit of mouthFrown*"),
    ("mouth/dimple", "mouth-corner lift added per unit of mouthDimple*"),
    ("mouth/stretch", "mouth width added per unit of mouthStretch*"),
    ("mouth/wide_smile", "mouth width added per unit of the mean corner lift (lifted corners only)"),
    ("mouth/wide_pucker", "mouth width lost per unit of mouthPucker"),
    ("mouth/round", "rounding per unit of mouthFunnel, 0..1"),
    ("mouth/pucker_start", "pucker threshold: pucker = (mouthPucker - start) x gain, 0..1"),
    ("mouth/pucker_gain", "gain on the pucker above its threshold; a kiss fades as the jaw opens"),
    ("mouth/shift_mouth", "sideways shift per unit of mouthLeft - mouthRight"),
    ("mouth/shift_jaw", "sideways shift per unit of jawLeft - jawRight"),
    ("mouth/press", "press per unit of the larger mouthPress*, 0..1 in total"),
    ("mouth/press_roll_lower", "press per unit of mouthRollLower"),
    ("mouth/press_roll_upper", "press per unit of mouthRollUpper"),
    ("mouth/press_close", "press per unit of mouthClose"),
    ("mouth/teeth_start", "upper-teeth threshold on mouthUpperUp*"),
    ("mouth/teeth_gain", "gain on the upper teeth above their threshold, 0..1"),
    ("mouth/teeth_lower_start", "lower-teeth threshold on mouthLowerDown*"),
    ("mouth/teeth_lower_gain", "gain on the lower teeth above their threshold"),
    ("mouth/teeth_lower_smile_start", "smile threshold for the lower teeth (lower teeth show on a real smile only)"),
    ("mouth/teeth_lower_smile_gain", "gain on the smile above that threshold"),
    ("cheeks/puff_blush", "blush per unit of cheekPuff"),
    ("cheeks/squint_blush", "blush per unit of cheekSquint"),
    ("cheeks/blush", "blush per unit of blush"),
    ("gaze/x", "sideways look per unit of the eye-look channels, clamped to -1..1"),
    ("gaze/y", "vertical look per unit of the eye-look channels, clamped to -1..1"),
    (
        "amplify/knee",
        "subtle-movement expander: each ARKit channel below this is multiplied by amplify.gain, then runs straight to \
         1 (0 turns it off), 0..0.5",
    ),
    ("amplify/gain", "the expander's boost below its knee, 1..4; knee x gain is at most 1"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_coefficient_has_exactly_one_formula() {
        let default = leaves(&to_json(&FaceMapping::default()).expect("the default mapping serialises"));
        let listed: Vec<&str> = FORMULAS.iter().map(|(p, _)| *p).collect();
        for path in default.keys() {
            assert!(formula(path).is_some(), "{path} has no formula");
        }
        assert_eq!(listed.len(), default.len(), "the table lists a path that FaceMapping doesn't have: {listed:?}");
        assert_eq!(default.len(), 60);
    }
}
