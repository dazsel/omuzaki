//! The character's clip set (#2414): `clip.set` and `clip.delete` write `characters/<id>/clips/<name>`, `clip.list`
//! lists the library's clips and a character's, and [`find_clip`] finds one for whatever plays it (`act.clip`, a short).
//!
//! A clip is authored from keyed poses: each key names a pose of the character's own set (`pose.set`) or of the
//! library, or gives one inline, with its hand shapes and reaches. Like a pose in the set, a stored clip is
//! **materialised**: each key's pose is copied in (mirrored when the key asks), so a later change to the library or the
//! pose set never changes the clip (ADR 0019).

use std::sync::Arc;

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{DocRead, Patch};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::clip::{ClipError, ClipKey, ClipLibrary, Easing, PoseClip};
use super::def::PoseDef;
use super::set::{PoseBody, did_you_mean, library_pose, pose_refusal, stored_poses};
use crate::hands::HandLibrary;

/// A key's foot contacts: `true` planted, `false` lifted; a side left out is the planter's own detection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactsBody {
    /// The left foot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<bool>,
    /// The right foot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<bool>,
}

impl ContactsBody {
    #[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
    fn is_empty(&self) -> bool {
        self.left.is_none() && self.right.is_none()
    }
}

/// One key as the set stores it: the time, the name the pose came from (for display) and the pose itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyBody {
    /// Seconds from the clip's start.
    pub t: f32,
    /// Where the pose came from: a pose name, with `mirrored` when the key mirrored it, or `inline`.
    pub from: String,
    /// The pose, copied in.
    pub pose: PoseBody,
    /// The foot contacts from this key on.
    #[serde(default, skip_serializing_if = "ContactsBody::is_empty")]
    pub contacts: ContactsBody,
}

/// A clip as the set stores it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipBody {
    /// One line saying what the clip is.
    pub summary: String,
    /// Seconds a play takes; a looped clip repeats with this period.
    pub length: f32,
    /// Whether the clip repeats.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub looped: bool,
    /// How each segment between keys is paced: `smooth`, `smoother` or `linear`.
    pub easing: String,
    /// The keys, in time order.
    pub keys: Vec<KeyBody>,
}

impl ClipBody {
    /// The clip this body describes, named `id`, checked as a library clip is.
    ///
    /// # Errors
    /// A refusal naming the key or field that is wrong.
    pub fn to_clip(&self, id: &str) -> Result<PoseClip, OpError> {
        let hands = HandLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
        let easing = Easing::from_name(&self.easing).ok_or_else(|| {
            OpError::invalid("easing", format!("there is no easing '{}'", self.easing))
                .hint("use `smooth` (stops at every key), `smoother`, or `linear` (not on a looped clip)")
        })?;
        let mut keys = Vec::with_capacity(self.keys.len());
        for (index, key) in self.keys.iter().enumerate() {
            let def = key.pose.clone().into_def(&format!("{id}-{index}")).map_err(|e| at_key(index, e))?;
            let posture = def.posture(hands).map_err(|e| at_key(index, pose_refusal(&e)))?;
            let contacts = [key.contacts.left, key.contacts.right];
            keys.push(ClipKey::new(key.t, key.from.clone(), posture, contacts));
        }
        PoseClip::new(id.to_owned(), self.summary.clone(), self.length, self.looped, easing, keys)
            .map_err(|e| clip_refusal(&e))
    }
}

/// `e` moved onto key `index`: its field is prefixed with `keys/<index>/`.
fn at_key(index: usize, e: OpError) -> OpError {
    let field = e.field.as_deref().map_or_else(|| format!("keys/{index}"), |f| format!("keys/{index}/{f}"));
    OpError { field: Some(field), ..e }
}

/// A clip check's refusal, on the field it names.
fn clip_refusal(e: &ClipError) -> OpError {
    let field = match e {
        ClipError::BadName { .. } => "id",
        ClipError::EmptySummary(_) => "summary",
        ClipError::BadLength { .. } => "length",
        ClipError::NoKeys(_) | ClipError::KeyOutside { .. } | ClipError::NotIncreasing { .. } => "keys",
        ClipError::UnknownEasing { .. } | ClipError::SeamNotSmooth { .. } => "easing",
        _ => "clip",
    };
    let error = OpError::invalid(field, e.to_string());
    match e {
        ClipError::KeyOutside { .. } => error.hint("every key's `t` is from 0 to `length` (below `length` when looped)"),
        ClipError::NotIncreasing { .. } => error.hint("list the keys in time order, each later than the one before"),
        ClipError::SeamNotSmooth { .. } => error.hint("a looped clip uses `smooth` or `smoother`"),
        _ => error,
    }
}

/// The character's clip set as the document holds it: `None` when the character has no clips yet.
fn stored_clips(ctx: &OpCtx<'_>, character: &CharId) -> Result<Option<Map<String, Value>>, OpError> {
    if ctx.doc.get(&paths::character(character)).is_none() {
        return Err(OpError::not_found(format!("there is no character '{character}'"))
            .hint("`character.create` makes one"));
    }
    match ctx.doc.get(&paths::clips(character)) {
        None => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map.clone())),
        Some(_) => Err(OpError::conflict(format!("{} is not an object, so it can't hold clips", paths::clips(character)))
            .hint("fix the character file")),
    }
}

/// The clip `id` of `character`'s own set, if it has one.
///
/// # Errors
/// A stored clip that doesn't read or doesn't pass the checks.
pub fn character_clip(doc: &dyn DocRead, character: &CharId, id: &str) -> Result<Option<PoseClip>, OpError> {
    let Some(value) = doc.get(&paths::clip(character, id)) else { return Ok(None) };
    let body = ClipBody::deserialize(value).map_err(|e| {
        OpError::invalid(format!("clips/{id}"), format!("the clip '{id}' of '{character}' doesn't read: {e}"))
            .hint("fix the character file, or store the clip again with `clip.set`")
    })?;
    body.to_clip(id).map(Some)
}

/// The clip `id` to play: `character`'s own clip of that name when it has one, else the library's.
///
/// # Errors
/// Not found, with the nearest name and where clips are listed; or a stored clip that doesn't read.
pub fn find_clip(doc: &dyn DocRead, character: Option<&CharId>, id: &str) -> Result<Arc<PoseClip>, OpError> {
    if let Some(character) = character
        && let Some(clip) = character_clip(doc, character, id)?
    {
        return Ok(Arc::new(clip));
    }
    let library = ClipLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
    if let Some(clip) = library.get(id) {
        return Ok(Arc::new(clip.clone()));
    }
    let mut names: Vec<String> = library.iter().map(|c| c.id.clone()).collect();
    if let Some(Value::Object(own)) = character.and_then(|c| doc.get(&paths::clips(c))) {
        names.extend(own.keys().cloned());
    }
    let error = match character {
        Some(c) => OpError::not_found(format!("there is no clip '{id}' in the library or in '{c}'s clip set")),
        None => OpError::not_found(format!("there is no clip '{id}' in the library")),
    };
    Err(match did_you_mean(id, names.iter().map(String::as_str)) {
        Some(near) => error.hint(format!("did you mean '{near}'? `clip.list` lists the clips")),
        None if character.is_none() => {
            error.hint("a character's own clip needs its `character`; `clip.list` lists the clips")
        }
        None => error.hint("`clip.list` lists the clips; `clip.set` makes one"),
    })
}

/// The pose a key gives: a name (the character's pose set first, then the library) or an inline pose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PoseRef {
    /// A pose of the character's own set (`pose.set`), or else a library pose (`pose.list`).
    Name(String),
    /// A pose given inline, as `pose.set` takes one.
    Inline(PoseBody),
}

/// One key of a clip being authored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyArg {
    /// Seconds from the clip's start: from 0 to `length` (below `length` when looped), each later than the last.
    pub t: f32,
    /// The pose at this key: a name (the character's own pose set first, then the library) or an inline pose.
    pub pose: PoseRef,
    /// Uses the mirror of the pose (sides swapped). Refused for a pose that is not mirrorable.
    #[serde(default)]
    pub mirror: bool,
    /// The foot contacts from this key on.
    #[serde(default)]
    pub contacts: ContactsBody,
}

/// `clip.set`: stores a clip in a character's set, from keyed poses (by name or inline) with their hand shapes and
/// reaches. Setting an existing name replaces it. `act.clip` and shorts play it by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "id": "hello", "summary": "A hand on the chest, then a wave.", "length": 1.6,
         "keys": [{"t": 0.0, "pose": "hand-on-chest"}, {"t": 1.6, "pose": "wave-right"}]},
        {"character": "kit", "id": "wave-loop", "summary": "A looping wave.", "length": 0.8, "looped": true,
         "keys": [{"t": 0.0, "pose": "wave-right"}, {"t": 0.4, "pose": "wave-right", "mirror": true}]}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetClip {
    /// The character whose set changes.
    pub character: CharId,
    /// The name the clip takes in the set, such as `hello`.
    pub id: String,
    /// One line saying what the clip is.
    pub summary: String,
    /// Seconds a play takes (up to an hour); a looped clip repeats with this period.
    pub length: f32,
    /// Whether the clip repeats. Defaults to false: it plays once and holds its last key.
    #[serde(default)]
    pub looped: bool,
    /// How each segment between keys is paced: `smooth` (the default; stops at every key), `smoother`, or `linear`
    /// (not on a looped clip).
    #[serde(default = "smooth")]
    pub easing: String,
    /// The keys, in time order: at least one.
    pub keys: Vec<KeyArg>,
}

fn smooth() -> String {
    "smooth".to_owned()
}

impl SetClip {
    /// Key `index`'s pose before mirroring: from the character's pose set, the library, or inline.
    fn key_pose(&self, ctx: &OpCtx<'_>, index: usize, key: &KeyArg) -> Result<(String, PoseDef), OpError> {
        let field = || format!("keys/{index}/pose");
        match &key.pose {
            PoseRef::Inline(body) => {
                let def = body.clone().into_def(&format!("{}-{index}", self.id)).map_err(|e| at_key(index, e))?;
                Ok(("inline".to_owned(), def))
            }
            PoseRef::Name(name) => {
                let own = stored_poses(ctx, &self.character)?.and_then(|set| set.get(name).cloned());
                let def = match own {
                    Some(value) => PoseBody::deserialize(&value)
                        .map_err(|e| OpError::invalid(field(), format!("the pose '{name}' in the set doesn't read: {e}")))?
                        .into_def(name)
                        .map_err(|e| at_key(index, e))?,
                    None => library_pose(name).map_err(|e| OpError { field: Some(field()), ..e })?,
                };
                Ok((name.clone(), def))
            }
        }
    }

    /// The clip as the set will store it, checked by building it.
    fn body(&self, ctx: &OpCtx<'_>) -> Result<ClipBody, OpError> {
        let mut keys = Vec::with_capacity(self.keys.len());
        for (index, key) in self.keys.iter().enumerate() {
            let (from, mut def) = self.key_pose(ctx, index, key)?;
            if key.mirror {
                def = def.mirrored().map_err(|e| OpError { field: Some(format!("keys/{index}/mirror")), ..pose_refusal(&e) })?;
            }
            let from = if key.mirror { format!("{from} mirrored") } else { from };
            keys.push(KeyBody { t: key.t, from, pose: PoseBody::of(&def), contacts: key.contacts });
        }
        let body = ClipBody {
            summary: self.summary.clone(),
            length: self.length,
            looped: self.looped,
            easing: self.easing.clone(),
            keys,
        };
        body.to_clip(&self.id)?;
        Ok(body)
    }
}

impl Op for SetClip {
    fn meta() -> OpMeta {
        OpMeta {
            name: "clip.set",
            summary: "Store a clip in a character's clip set from keyed poses (by name or inline, with hand shapes and reaches); act.clip and shorts play it",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let clips = stored_clips(ctx, &self.character)?;
        let body = self.body(ctx)?;
        let value =
            serde_json::to_value(&body).map_err(|e| OpError::internal(format!("storing the clip failed: {e}")))?;
        let before = clips.as_ref().and_then(|map| map.get(&self.id)).cloned();
        let patch = match clips {
            // A character with no set yet gets the set in one change: the document never creates parents on its own.
            None => Patch::new().with(
                paths::clips(&self.character),
                None,
                Some(Value::Object(Map::from_iter([(self.id.clone(), value)]))),
            ),
            Some(_) => Patch::new().with(paths::clip(&self.character, &self.id), before.clone(), Some(value)),
        };
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({
            "id": self.id, "created": before.is_none(), "keys": body.keys.len(), "length": body.length, "looped": body.looped
        }));
        Ok(plan)
    }
}

register_op!(SetClip);

/// `clip.delete`: removes a clip from a character's set. The library is not touched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"character": "kit", "id": "hello"}], "x-omuzaki-fixture" = "character-clip"))]
pub struct DeleteClip {
    /// The character whose set changes.
    pub character: CharId,
    /// The clip to remove.
    pub id: String,
}

impl Op for DeleteClip {
    fn meta() -> OpMeta {
        OpMeta {
            name: "clip.delete",
            summary: "Remove a clip from a character's clip set (the library is not changed)",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let clips = stored_clips(ctx, &self.character)?.unwrap_or_default();
        let Some(before) = clips.get(&self.id).cloned() else {
            let mut names: Vec<&str> = clips.keys().map(String::as_str).collect();
            names.sort_unstable();
            let hint = match (names.is_empty(), did_you_mean(&self.id, names.iter().copied())) {
                (true, _) => "the character has no clips of its own; `clip.set` stores one".to_owned(),
                (false, Some(near)) => format!("did you mean `{near}`?"),
                (false, None) => format!("its clips: {}", names.join(", ")),
            };
            return Err(
                OpError::not_found(format!("there is no clip '{}' in this character's set", self.id)).hint(hint)
            );
        };
        let patch = Patch::new().with(paths::clip(&self.character, &self.id), Some(before), None);
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "id": self.id }));
        Ok(plan)
    }
}

register_op!(DeleteClip);

/// `clip.list`: the library's clips, and a character's own when it is named, with what each is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, {"character": "kit"}], "x-omuzaki-fixture" = "character-clip"))]
pub struct ListClips {
    /// Also list this character's own clips (`clip.set`), which play before a library clip of the same name.
    #[serde(default)]
    pub character: Option<CharId>,
}

impl Op for ListClips {
    fn meta() -> OpMeta {
        OpMeta {
            name: "clip.list",
            summary: "List the library's body clips, and a character's own clips, with id, summary, length and looping",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let library = ClipLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
        let describe = |c: &PoseClip| {
            json!({ "id": c.id, "summary": c.summary, "length": c.length(), "looped": c.looped(), "keys": c.keys().len() })
        };
        let listed: Vec<Value> = library.iter().map(describe).collect();
        let Some(character) = &self.character else { return Ok(Plan::answer(json!({ "library": listed }))) };
        let mut own = Vec::new();
        let set = stored_clips(ctx, character)?.unwrap_or_default();
        let mut names: Vec<&String> = set.keys().collect();
        names.sort();
        for name in names {
            if let Some(clip) = character_clip(ctx.doc, character, name)? {
                own.push(describe(&clip));
            }
        }
        Ok(Plan::answer(json!({ "library": listed, "character": own })))
    }
}

register_op!(ListClips);
