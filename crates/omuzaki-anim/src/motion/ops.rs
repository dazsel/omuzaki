//! `motion.import`: a motion-capture take (the CMU library's `.asf` and `.amc`, or any Acclaim pair) retargeted onto the
//! VRM humanoid and stored in a character's clip set, so `act.clip` plays it live over the body and shorts play it by
//! name, like any clip `clip.set` makes.
//!
//! The take is stored **materialised**, as `clip.set` stores a clip: one inline key pose every `1 / key_fps` seconds
//! with linear pacing, so playing it at a key's time gives the take's own pose there, exactly. Nothing reads the files
//! again: the clip is the document's (ADR 0019).
//!
//! The files are read through the op's file access (`OpCtx::fs_access`), so the actor's grants and the project folder
//! apply; the text can also come inline, which is how an agent hands over a take it holds.

use std::io::Read as _;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{CharId, paths};
use omuzaki_core::glam::Quat;
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{FsMode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::HumanoidTake;
use super::asf::Skeleton;
use crate::poses::{ClipBody, ContactsBody, HandBody, HandsBody, KeyBody, PoseBody, ReachesBody};

/// The largest motion file read, bytes: a CMU take of several minutes is a few megabytes.
const MAX_FILE_BYTES: u64 = 64 << 20;
/// The longest clip an import stores, seconds: keys are inline, so the clip's size grows with its length.
pub const MAX_IMPORT_S: f32 = 120.0;
/// The most keys a second an import stores.
pub const MAX_KEY_FPS: f32 = 60.0;
/// The keys a second when the call gives none: one per frame of a 30 fps stream.
const DEFAULT_KEY_FPS: f32 = 30.0;
/// The CMU library's recording rate.
const DEFAULT_FPS: f32 = 120.0;
/// The hips height the take's travel is scaled to when the call gives none, metres: the kit's standard body.
const DEFAULT_HIPS_HEIGHT: f32 = 0.9;
/// A rotation this close to the T-pose is left out of a key (the bone stays at rest).
const AT_REST: f32 = 1e-7;

/// Where a motion file's text comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MotionText {
    /// A file, read through the op's file access (a relative path is in the project folder).
    Path(String),
    /// The file's text itself.
    Text(String),
}

impl MotionText {
    fn read(&self, ctx: &OpCtx<'_>, field: &str) -> Result<String, OpError> {
        match self {
            Self::Text(text) => Ok(text.clone()),
            Self::Path(path) => {
                let file = ctx
                    .fs_access(path, FsMode::Read)
                    .map_err(|e| OpError { field: Some(format!("{field}/path")), ..e })?
                    .open_read()?;
                let mut text = String::new();
                file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text).map_err(|e| {
                    OpError::invalid(format!("{field}/path"), format!("{path} doesn't read as text: {e}"))
                })?;
                if text.len() as u64 > MAX_FILE_BYTES {
                    return Err(OpError::invalid(
                        format!("{field}/path"),
                        format!("{path} is larger than {} MB", MAX_FILE_BYTES >> 20),
                    ));
                }
                Ok(text)
            }
        }
    }
}

/// Retarget a motion-capture take (Acclaim `.asf` skeleton and `.amc` motion, the CMU library's format) onto the
/// humanoid and store it in a character's clip set; `act.clip` then plays it live and shorts play it by name. Bones the
/// skeleton lacks stay straight on their parents; fingers stay as `hands` says (the CMU takes record none).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "id": "turn", "summary": "A quarter turn of the hips.",
         "skeleton": {"text": ":units\n  length 0.45\n"},
         "motion": {"text": ":DEGREES\n1\nroot 0 0 0 0 0 0\n2\nroot 0 0 0 0 30 0\n"}, "fps": 1.0},
        {"character": "kit", "id": "talk", "skeleton": {"path": "mocap/18.asf"}, "motion": {"path": "mocap/18_08.amc"},
         "from_s": 2.0, "length_s": 8.0, "hands": "rest"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct ImportMotion {
    /// The character whose clip set takes the clip.
    pub character: CharId,
    /// The clip's name in the set, such as `talk`. An existing clip of that name is replaced.
    pub id: String,
    /// One line saying what the clip is. Default: where it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The skeleton (`.asf`).
    pub skeleton: MotionText,
    /// The motion on it (`.amc`).
    pub motion: MotionText,
    /// Frames a second the motion was recorded at. Default 120 (the CMU library).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f32>,
    /// Seconds into the take the clip starts. Default 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_s: Option<f32>,
    /// Seconds of it to keep, up to 120. Default: the rest of the take, up to 120.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_s: Option<f32>,
    /// Keys a second, 1 to 60. Default 30. Played at a key's time the clip is the take exactly; between keys it is
    /// blended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_fps: Option<f32>,
    /// Keep the hips' travel across the floor. Default `false`: the hips stay over the spot (rising and sinking only),
    /// which is what a streamer's framing wants.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub travel: bool,
    /// The hips height the travel is scaled to, metres. Default 0.9.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hips_height: Option<f32>,
    /// A hand shape held on both hands through the clip (`hand.list` lists them; `rest` is relaxed). Default none:
    /// the fingers stay straight, as the take has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hands: Option<String>,
}

/// `value` as a number in `lo..=hi`, refused on `field` otherwise.
fn within(field: &str, value: f32, lo: f32, hi: f32) -> Result<f32, OpError> {
    if value.is_finite() && (lo..=hi).contains(&value) {
        Ok(value)
    } else {
        Err(OpError::invalid(field, format!("{field} is {value}; it must be from {lo} to {hi}")))
    }
}

impl ImportMotion {
    /// The take, retargeted and placed as the call asks.
    fn take(&self, ctx: &OpCtx<'_>) -> Result<HumanoidTake, OpError> {
        let fps = within("fps", self.fps.unwrap_or(DEFAULT_FPS), 1.0, 1000.0)?;
        let hips = within("hips_height", self.hips_height.unwrap_or(DEFAULT_HIPS_HEIGHT), 0.1, 10.0)?;
        let skeleton = Skeleton::parse(&self.skeleton.read(ctx, "skeleton")?)
            .map_err(|e| OpError::invalid("skeleton", e.to_string()).hint("an Acclaim .asf skeleton"))?;
        let take = skeleton
            .motion(&self.motion.read(ctx, "motion")?, fps, hips)
            .map_err(|e| OpError::invalid("motion", e.to_string()).hint("an Acclaim .amc motion of that skeleton"))?;
        Ok(if self.travel { take } else { take.in_place() })
    }

    /// The clip as the set will store it.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // bounded counts
    fn body(&self, take: &HumanoidTake) -> Result<ClipBody, OpError> {
        let seconds = take.seconds();
        let from = within("from_s", self.from_s.unwrap_or(0.0), 0.0, seconds)?;
        let length = match self.length_s {
            Some(length) => within("length_s", length, 0.0, MAX_IMPORT_S)?.min(seconds - from),
            None => (seconds - from).min(MAX_IMPORT_S),
        };
        let key_fps = within("key_fps", self.key_fps.unwrap_or(DEFAULT_KEY_FPS), 1.0, MAX_KEY_FPS)?;
        let hands = self.hands.as_ref().map(|shape| HandsBody {
            left: Some(HandBody { shape: shape.clone(), weight: 1.0 }),
            right: Some(HandBody { shape: shape.clone(), weight: 1.0 }),
        });
        let steps = (length * key_fps).floor() as usize;
        let mut times: Vec<f32> = (0..=steps).map(|k| k as f32 / key_fps).collect();
        if times.last().is_some_and(|t| length - t > 1e-4) {
            times.push(length);
        }
        let keys = times
            .into_iter()
            .map(|t| {
                let pose = take.sample(from + t);
                let bones = HumanoidBone::ALL
                    .iter()
                    .map(|bone| (*bone, pose.get(*bone)))
                    .filter(|(_, q)| !q.abs_diff_eq(Quat::IDENTITY, AT_REST))
                    .map(|(bone, q)| (bone.name().to_owned(), q.to_array()))
                    .collect();
                KeyBody {
                    t,
                    from: "inline".to_owned(),
                    pose: PoseBody {
                        summary: format!("{} at {:.3} s", self.id, from + t),
                        tags: vec![],
                        mirrorable: true,
                        hips_offset: pose.hips_offset.to_array(),
                        bones,
                        hands: hands.clone().unwrap_or_default(),
                        reach: ReachesBody::default(),
                    },
                    contacts: ContactsBody::default(),
                }
            })
            .collect();
        let summary = self
            .summary
            .clone()
            .unwrap_or_else(|| format!("Motion capture, {length:.1} s from {from:.1} s, as {}", self.id));
        // A one-frame take is a held pose: a clip needs a length above zero.
        let body =
            ClipBody { summary, length: length.max(1.0 / key_fps), looped: false, easing: "linear".to_owned(), keys };
        body.to_clip(&self.id)?;
        Ok(body)
    }
}

impl Op for ImportMotion {
    fn meta() -> OpMeta {
        OpMeta {
            name: "motion.import",
            summary: "Retarget a motion-capture take (.asf and .amc) onto the humanoid and store it as a character's clip; act.clip plays it live",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("`character.create` makes one"));
        }
        let clips = match ctx.doc.get(&paths::clips(&self.character)) {
            None => None,
            Some(Value::Object(map)) => Some(map.clone()),
            Some(_) => {
                return Err(OpError::conflict(format!("{} is not an object", paths::clips(&self.character)))
                    .hint("fix the character file"));
            }
        };
        let take = self.take(ctx)?;
        let body = self.body(&take)?;
        let value =
            serde_json::to_value(&body).map_err(|e| OpError::internal(format!("storing the clip failed: {e}")))?;
        let before = clips.as_ref().and_then(|map| map.get(&self.id)).cloned();
        let patch = match clips {
            None => Patch::new().with(
                paths::clips(&self.character),
                None,
                Some(Value::Object(Map::from_iter([(self.id.clone(), value)]))),
            ),
            Some(_) => Patch::new().with(paths::clip(&self.character, &self.id), before.clone(), Some(value)),
        };
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({
            "id": self.id,
            "created": before.is_none(),
            "keys": body.keys.len(),
            "length": body.length,
            "take_s": take.seconds(),
            "plays_with": {"op": "act.clip", "args": {"clip": self.id, "character": self.character}},
        }));
        Ok(plan)
    }
}

register_op!(ImportMotion);
