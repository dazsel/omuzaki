//! The living body's query ops: `gesture.list`, `body.styles` and `measure.body_motion`. The live dial
//! (`perform.set_body`) is the perform host's, in `omuzaki-stage`, beside `perform.set_retarget`.

use omuzaki_anim_api::{BodyStyle, HumanoidPose, Personality};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::gesture::GestureLibrary;
use super::measure::{Take, measure, measure_face_only};
use super::tune::Tuning;
use crate::poses::PoseLibrary;

/// Longest measured take, seconds.
const MAX_SECONDS: f32 = 600.0;

/// List the co-speech gestures and idle fidgets the living body plays, with their class and hands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListGestures {}

impl Op for ListGestures {
    fn meta() -> OpMeta {
        OpMeta {
            name: "gesture.list",
            summary: "List the co-speech gestures and idle fidgets the living body plays (beats, points, offers, shrugs, ...), with their class and hands.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let library = GestureLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
        let gestures: Vec<Value> = library
            .iter()
            .map(|g| json!({ "id": g.id, "summary": g.summary, "kind": g.kind, "hands": g.hands, "strokes": g.strokes }))
            .collect();
        Ok(Plan::answer(json!({ "gestures": gestures })))
    }
}

omuzaki_ops_api::register_op!(ListGestures);

/// List the living body's presets and what each does: sway, weight shifts, how much the chest follows the head,
/// gesture size and rate, speed, posture. `perform.set_body` picks one and scales its parts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListBodyStyles {}

const PRESETS: [Personality; 5] =
    [Personality::Natural, Personality::Energetic, Personality::Calm, Personality::Shy, Personality::Sleepy];

fn describe(t: &Tuning) -> Value {
    let round = |v: f32| (f64::from(v) * 100.0).round() / 100.0;
    json!({
        "sway_deg": round(t.sway_deg),
        "weight_shift_every_s": [round(t.shift_gap_s.lo), round(t.shift_gap_s.hi)],
        "chest_follows_head": t.follow.map(round),
        "gesture_size": round(t.gesture_size),
        "gesture_gap_s": [round(t.gesture_gap_s.lo), round(t.gesture_gap_s.hi)],
        "speed": round(t.speed),
        "slump_deg": round(t.posture_deg[0]),
        "shoulders_in_deg": round(t.posture_deg[1]),
        "fidget_every_s": [round(t.fidget_gap_s.lo), round(t.fidget_gap_s.hi)],
    })
}

impl Op for ListBodyStyles {
    fn meta() -> OpMeta {
        OpMeta {
            name: "body.styles",
            summary: "List the living body's presets (natural, energetic, calm, shy, sleepy) with their sway, gesture size and rate, speed and posture, and the dials perform.set_body takes.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let presets: Vec<Value> = PRESETS
            .iter()
            .map(|p| json!({ "preset": p, "tuning": describe(&Tuning::of(&BodyStyle::preset(*p))) }))
            .collect();
        let dials: Vec<Value> = BodyStyle::default().amounts().iter().map(|(name, _)| json!(name)).collect();
        Ok(Plan::answer(json!({ "presets": presets, "dials": dials, "default": BodyStyle::default() })))
    }
}

omuzaki_ops_api::register_op!(ListBodyStyles);

/// Measure how alive a body style looks on a synthetic talking take, against the face-only body on the same take:
/// frozen 0.5 s windows, motion by region, the fastest turn, gestures a minute and how close each stroke lands to its
/// stressed syllable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MeasureBodyMotion {
    /// The style to measure; the natural preset when left out.
    #[serde(default)]
    pub style: Option<BodyStyle>,
    /// How long the take is, seconds (at 60 fps). Default 60.
    #[serde(default = "default_seconds")]
    pub seconds: f32,
    /// Share of the take spent talking, `0..=1`. Default 0.7.
    #[serde(default = "default_talk")]
    pub talk_share: f32,
    /// Seeds the take and the body's choices. Default 7.
    #[serde(default = "default_seed")]
    pub seed: u64,
}

fn default_seconds() -> f32 {
    60.0
}
fn default_talk() -> f32 {
    0.7
}
fn default_seed() -> u64 {
    7
}

/// The base pose a measured body stands in: the pose library's `stand-relaxed`, arms down at the sides.
pub fn standing_base() -> HumanoidPose {
    PoseLibrary::builtin()
        .ok()
        .and_then(|lib| lib.get("stand-relaxed"))
        .and_then(|p| p.to_humanoid().ok())
        .unwrap_or(HumanoidPose::REST)
}

impl Op for MeasureBodyMotion {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.body_motion",
            summary: "Measure a living-body style on a synthetic talking take against the face-only body: frozen 0.5 s windows, motion by region, gestures a minute and stroke-to-stress timing.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(self.seconds.is_finite() && self.seconds >= 1.0 && self.seconds <= MAX_SECONDS) {
            return Err(OpError::invalid("seconds", "must be 1 to 600"));
        }
        if !(0.0..=1.0).contains(&self.talk_share) {
            return Err(OpError::invalid("talk_share", "must be 0 to 1"));
        }
        let style = self.style.unwrap_or_default().clamped();
        let take = Take::synthesize(self.seconds, self.talk_share, self.seed);
        let base = standing_base();
        let living = measure(&style, &take, &base, self.seed);
        let face_only = measure_face_only(&take, &base);
        Ok(Plan::answer(json!({
            "style": style,
            "stressed_syllables": take.onsets.len(),
            "living": living,
            "face_only": face_only,
        })))
    }
}

omuzaki_ops_api::register_op!(MeasureBodyMotion);
