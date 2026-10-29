//! The character's pose set (RG-06, `docs/design/library.md` §4.7, ADR 0019): `pose.set` and `pose.delete` write
//! `characters/<id>/poses/<name>`, and `pose.list_character` lists it.
//!
//! A pose in the set is **materialised**: its rotations are copied in, not referenced, so a later change to the library
//! never changes the character. The stored form is [`PoseBody`], the same form an inline `pose` argument takes. Rotations
//! there are unit quaternions `[x, y, z, w]`, which round-trip exactly; the library file's degrees would not.

use std::collections::BTreeMap;

use omuzaki_anim_api::{HumanoidBone, Reach, Reaches};
use omuzaki_char_api::{CharId, paths};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::text::{edit_distance, nearest};
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::def::{HandSlot, PoseDef, PoseError};
use super::library::PoseLibrary;
use crate::hands::HandLibrary;

/// How far a rotation's length may be from 1. Library rotations are unit to a few parts in ten million; this allows for an
/// author rounding to a few decimals, and refuses a quaternion that scales the bone.
const UNIT_TOLERANCE: f32 = 1e-3;

/// A pose as the set stores it and as an inline `pose` argument gives it: the fields of a library pose, with each
/// rotation a unit quaternion `[x, y, z, w]` keyed by its VRM bone name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoseBody {
    /// One line saying what the pose is, for `pose.list_character`.
    pub summary: String,
    /// Free labels (`greeting`, `idle`) to search by.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Whether `mirror` may reflect the pose. Defaults to true.
    #[serde(default = "yes")]
    pub mirrorable: bool,
    /// Hips position relative to the rest hips, metres.
    #[serde(default)]
    pub hips_offset: [f32; 3],
    /// Parent-local rotation of each bone that is not at the T-pose, as a unit quaternion `[x, y, z, w]` in the VRM
    /// normalised space. A bone not listed stays at the T-pose.
    #[serde(default)]
    pub bones: BTreeMap<String, [f32; 4]>,
    /// The hand shape on each side, if the pose sets one.
    #[serde(default)]
    pub hands: HandsBody,
    /// Where each hand reaches on the body, if the pose says (#2414).
    #[serde(default, skip_serializing_if = "ReachesBody::is_empty")]
    pub reach: ReachesBody,
}

/// The `reach` of a pose: one optional reach per side. A side that is absent is not written.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReachesBody {
    /// The character's left hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<ReachBody>,
    /// The character's right hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<ReachBody>,
}

impl ReachesBody {
    /// Whether neither side reaches.
    pub fn is_empty(&self) -> bool {
        self.left.is_none() && self.right.is_none()
    }
}

/// One hand's reach: a point on a bone of the body, in arm lengths (`omuzaki_anim_api::reach`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReachBody {
    /// The bone the target rides on (a VRM humanoid bone name, such as `upperChest`).
    pub bone: HumanoidBone,
    /// The wrist's target from the bone's joint, in arm lengths: `+x` the character's left, `+y` up, `+z` forward,
    /// turned with the bone.
    pub at: [f32; 3],
    /// The side the elbow bends to, in the same frame. Defaults to down and a little behind.
    #[serde(default = "super::def::default_pole")]
    pub pole: [f32; 3],
    /// `0..=1`; defaults to full.
    #[serde(default = "full_weight")]
    pub weight: f32,
}

fn reach_body(reach: &Reach) -> ReachBody {
    ReachBody { bone: reach.bone, at: reach.offset.to_array(), pole: reach.pole.to_array(), weight: reach.weight }
}

fn reach_of(body: &ReachBody) -> Reach {
    Reach { bone: body.bone, offset: Vec3::from_array(body.at), pole: Vec3::from_array(body.pole), weight: body.weight }
}

/// The `hands` of a pose: one optional shape per side. A side that is absent is not written, so the document never
/// holds a `null`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandsBody {
    /// The character's left hand shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<HandBody>,
    /// The character's right hand shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<HandBody>,
}

/// One hand: a shape id from the hand-shape library and how strongly it applies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandBody {
    /// A shape id, such as `open` or `fist`.
    pub shape: String,
    /// `0..=1`; defaults to full.
    #[serde(default = "full_weight")]
    pub weight: f32,
}

fn yes() -> bool {
    true
}

fn full_weight() -> f32 {
    1.0
}

impl PoseBody {
    /// The stored form of a checked pose.
    pub(crate) fn of(def: &PoseDef) -> Self {
        Self {
            summary: def.summary.clone(),
            tags: def.tags.clone(),
            mirrorable: def.mirrorable,
            hips_offset: def.hips_offset.to_array(),
            bones: def.bones.iter().map(|(bone, q)| (bone.name().to_owned(), q.to_array())).collect(),
            hands: HandsBody {
                left: def.left_hand.as_ref().map(hand_body),
                right: def.right_hand.as_ref().map(hand_body),
            },
            reach: ReachesBody { left: def.reach.left.as_ref().map(reach_body), right: def.reach.right.as_ref().map(reach_body) },
        }
    }

    /// The pose this body describes, named `id`. Each bone and its rotation are checked where they enter, so a refusal
    /// names the bone; the rest is checked as a library file is.
    pub(crate) fn into_def(self, id: &str) -> Result<PoseDef, OpError> {
        let mut bones = BTreeMap::new();
        for (name, q) in &self.bones {
            let field = format!("pose/bones/{name}");
            let bone = HumanoidBone::from_name(name).ok_or_else(|| unknown_bone(&field, name))?;
            check_rotation(&field, q)?;
            bones.insert(bone, Quat::from_array(*q));
        }
        let def = PoseDef {
            id: id.to_owned(),
            summary: self.summary,
            tags: self.tags,
            bones,
            hips_offset: Vec3::from_array(self.hips_offset),
            left_hand: self.hands.left.map(hand_slot),
            right_hand: self.hands.right.map(hand_slot),
            reach: Reaches { left: self.reach.left.as_ref().map(reach_of), right: self.reach.right.as_ref().map(reach_of) },
            mirrorable: self.mirrorable,
        };
        checked(&def)?;
        Ok(def)
    }
}

fn hand_body(slot: &HandSlot) -> HandBody {
    HandBody { shape: slot.shape.clone(), weight: slot.weight }
}

fn hand_slot(body: HandBody) -> HandSlot {
    HandSlot { shape: body.shape, weight: body.weight }
}

/// Refuses a rotation with a value that is not finite, or whose length is not 1. The message names the field.
fn check_rotation(field: &str, q: &[f32; 4]) -> Result<(), OpError> {
    if let Some(v) = q.iter().find(|v| !v.is_finite()) {
        return Err(OpError::invalid(field, format!("{field} has {v}, which is not a finite number")));
    }
    let length = Quat::from_array(*q).length();
    if (length - 1.0).abs() > UNIT_TOLERANCE {
        return Err(OpError::invalid(field, format!("{field} is not a unit quaternion: its length is {length}, not 1"))
            .hint("write the rotation as a unit quaternion [x, y, z, w]; `from` copies a library pose, which is already one"));
    }
    Ok(())
}

/// Refuses a bone name the humanoid does not have, with the nearest name when it is a typo.
fn unknown_bone(field: &str, name: &str) -> OpError {
    let hint = match did_you_mean(name, HumanoidBone::ALL.iter().map(|b| b.name())) {
        Some(near) => format!("did you mean `{near}`?"),
        None => "bones use the VRM 1.0 names, such as `leftUpperArm`".to_owned(),
    };
    OpError::invalid(field, format!("there is no bone called '{name}'")).hint(hint)
}

/// The candidate nearest to `wanted`, when it is close enough to be a typo: at most a third of its length plus one edit
/// away. The candidates are scanned in the order given, so a tie goes to the first.
pub(crate) fn did_you_mean<'a>(wanted: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let close = wanted.chars().count() / 3 + 1;
    nearest(wanted, candidates).filter(|near| edit_distance(wanted, near) <= close)
}

/// A pose check as the library runs it: the id and tags are names, the numbers are finite, and every hand shape exists.
fn checked(def: &PoseDef) -> Result<(), OpError> {
    def.validate().map_err(|e| pose_refusal(&e))?;
    let hands = HandLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
    def.posture(hands).map(drop).map_err(|e| pose_refusal(&e))
}

/// A library-style refusal: the message already names the pose and the field that is wrong.
pub(crate) fn pose_refusal(e: &PoseError) -> OpError {
    OpError::invalid("pose", e.to_string())
}

/// The library pose called `from`, as the set will hold it.
pub(crate) fn library_pose(from: &str) -> Result<PoseDef, OpError> {
    let library = PoseLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
    library.get(from).cloned().ok_or_else(|| {
        let hint = match did_you_mean(from, library.iter().map(|p| p.id.as_str())) {
            Some(near) => format!("did you mean `{near}`?"),
            None => "`pose.list` lists the library poses".to_owned(),
        };
        OpError::not_found(format!("there is no library pose '{from}'")).hint(hint)
    })
}

/// The character's pose set as the document holds it: `None` when the character has no poses yet.
pub(crate) fn stored_poses(ctx: &OpCtx<'_>, character: &CharId) -> Result<Option<Map<String, Value>>, OpError> {
    if ctx.doc.get(&paths::character(character)).is_none() {
        return Err(OpError::not_found(format!("there is no character '{character}'"))
            .hint("`character.create` makes one"));
    }
    match ctx.doc.get(&paths::poses(character)) {
        None => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map.clone())),
        Some(_) => Err(OpError::conflict(format!(
            "{} is not an object, so it can't hold poses",
            paths::poses(character)
        ))
        .hint("fix the character file")),
    }
}

/// A pose called `id` that the set lacks, with the name it may have meant.
fn missing_pose<'a>(id: &str, names: impl IntoIterator<Item = &'a String>) -> OpError {
    let mut names: Vec<&str> = names.into_iter().map(String::as_str).collect();
    names.sort_unstable();
    let hint = match (names.is_empty(), did_you_mean(id, names.iter().copied())) {
        (true, _) => "`pose.set` stores one".to_owned(),
        (false, Some(near)) => format!("did you mean `{near}`?"),
        (false, None) => "`pose.list_character` lists the character's poses".to_owned(),
    };
    OpError::not_found(format!("there is no pose '{id}' in this character's set")).hint(hint)
}

/// `pose.set`: stores a pose in a character's set, copied from the library (`from`) or given inline (`pose`), mirrored
/// when `mirror` is set. Setting an existing name replaces it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "id": "wave", "from": "wave-right"},
        {"character": "kit", "id": "wave-left", "from": "wave-right", "mirror": true}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetPose {
    /// The character whose set changes.
    pub character: CharId,
    /// The name the pose takes in the set, such as `wave`.
    pub id: String,
    /// A library pose id, such as `wave-right`. Give this or `pose`, not both.
    #[serde(default)]
    pub from: Option<String>,
    /// An inline pose. Give this or `from`, not both.
    #[serde(default)]
    pub pose: Option<PoseBody>,
    /// Stores the mirror of the pose (sides swapped). Refused for a pose that is not mirrorable.
    #[serde(default)]
    pub mirror: bool,
}

impl SetPose {
    /// The pose the set will hold, before mirroring: from the library or inline, named `id`, and checked.
    fn source(&self) -> Result<PoseDef, OpError> {
        let mut def = match (&self.from, &self.pose) {
            (Some(from), None) => {
                let mut def = library_pose(from)?;
                def.id.clone_from(&self.id);
                checked(&def)?;
                def
            }
            (None, Some(body)) => body.clone().into_def(&self.id)?,
            (Some(_), Some(_)) => {
                return Err(OpError::invalid(
                    "from/pose",
                    "give `from` or `pose`, not both: `from` copies a library pose and `pose` is inline",
                ));
            }
            (None, None) => {
                return Err(OpError::invalid("from/pose", "give one of `from` (a library pose id) or `pose` (inline)"));
            }
        };
        if self.mirror {
            def = def.mirrored().map_err(|e| pose_refusal(&e))?;
        }
        Ok(def)
    }
}

impl Op for SetPose {
    fn meta() -> OpMeta {
        OpMeta {
            name: "pose.set",
            summary: "Store a pose in a character's pose set, copied from the library (from) or inline (pose), mirrored with mirror",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = self.source()?;
        let value = serde_json::to_value(PoseBody::of(&def))
            .map_err(|e| OpError::internal(format!("storing the pose failed: {e}")))?;
        let poses = stored_poses(ctx, &self.character)?;
        let before = poses.as_ref().and_then(|map| map.get(&self.id)).cloned();
        let patch = match poses {
            // A character with no set yet gets the set in one change: the document never creates parents on its own.
            None => Patch::new().with(
                paths::poses(&self.character),
                None,
                Some(Value::Object(Map::from_iter([(self.id.clone(), value)]))),
            ),
            Some(_) => Patch::new().with(paths::pose(&self.character, &self.id), before.clone(), Some(value)),
        };
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "id": self.id, "created": before.is_none(), "mirrored": self.mirror }));
        Ok(plan)
    }
}

register_op!(SetPose);

/// `pose.delete`: removes a pose from a character's set. The library is not touched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "id": "wave"}
    ],
    "x-omuzaki-fixture" = "character-pose"
))]
pub struct DeletePose {
    /// The character whose set changes.
    pub character: CharId,
    /// The pose to remove.
    pub id: String,
}

impl Op for DeletePose {
    fn meta() -> OpMeta {
        OpMeta {
            name: "pose.delete",
            summary: "Remove a pose from a character's pose set (the library is not changed)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let poses = stored_poses(ctx, &self.character)?.unwrap_or_default();
        let Some(before) = poses.get(&self.id).cloned() else {
            return Err(missing_pose(&self.id, poses.keys()));
        };
        let patch = Patch::new().with(paths::pose(&self.character, &self.id), Some(before), None);
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "id": self.id }));
        Ok(plan)
    }
}

register_op!(DeletePose);

/// `pose.list_character`: the poses in a character's own set, in name order, so an agent sees what a render op finds
/// before it falls back to the library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"}
    ],
    "x-omuzaki-fixture" = "character-pose"
))]
pub struct ListCharacterPoses {
    /// The character whose set is listed.
    pub character: CharId,
}

impl Op for ListCharacterPoses {
    fn meta() -> OpMeta {
        OpMeta {
            name: "pose.list_character",
            summary: "List a character's own pose set (name, summary, tags), in name order",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let poses = stored_poses(ctx, &self.character)?.unwrap_or_default();
        let mut entries: Vec<(&String, &Value)> = poses.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let listed = entries
            .into_iter()
            .map(|(name, value)| {
                let body = PoseBody::deserialize(value).map_err(|e| {
                    OpError::invalid(
                        format!("poses/{name}"),
                        format!("the pose '{name}' in the document doesn't read: {e}"),
                    )
                    .hint("fix the character file: a pose needs a summary, and its bones are unit quaternions")
                })?;
                Ok(json!({ "id": name, "summary": body.summary, "tags": body.tags, "mirrorable": body.mirrorable }))
            })
            .collect::<Result<Vec<_>, OpError>>()?;
        Ok(Plan::answer(json!({ "poses": listed })))
    }
}

register_op!(ListCharacterPoses);
