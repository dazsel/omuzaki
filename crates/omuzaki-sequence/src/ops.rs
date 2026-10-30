//! The `sequence.*` ops. A sequence lives in the project at `sequences/<id>` (or in a `*.seq.toml` file, or inline), so
//! a person in the panels and an agent over MCP edit the same one, every edit undoable:
//!
//! - edits: `sequence.set` (create or replace one whole), `sequence.key` (set or remove one key of any track, a pose or
//!   a face key, by its path), `sequence.delete`;
//! - queries: `sequence.get`, `sequence.list`, `sequence.check` (the shots, layers and frames, and what a render would
//!   refuse) and `sequence.kinds` (every easing, layer kind, icon, volume preset, prop kind, pose, hand shape and
//!   expression a sequence may name);
//! - jobs: `sequence.render` (a WebM video) and `sequence.stills` (frames at chosen times, as one PNG sheet).

use std::io::Read as _;
use std::sync::Mutex;

use omuzaki_anim::hands::HandLibrary;
use omuzaki_anim::poses::PoseLibrary;
use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, paths as char_paths};
use omuzaki_core::id::Slug;
use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::{DocPath, DocRead, Patch, Value};
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops_api::{CostHint, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Capabilities, MtoonSupport, Renderer};
use omuzaki_short::encode::ShortWriter;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_stage::ops::render::job::{RenderQuality, character_bytes, check_size, cost};
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::scene::alone::{StagedScene, cast_alone, occlusion_in_look, post_in_look, stage_alone};
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage_api::SceneId;
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::curve::{Curve, Ease};
use crate::def::{SequenceDef, Shot};
use crate::error::SequenceError;
use crate::layer::ICONS;
use crate::render::{Collect, ShotStage, render_frames};
use crate::volume::VolumePreset;

/// The largest sequence file read, bytes.
const MAX_FILE_BYTES: u64 = 4 << 20;
/// Report progress every this many frames.
const PROGRESS_EVERY: usize = 12;
/// The most stills in one sheet.
pub const MAX_STILLS: usize = 48;
/// Seconds per frame of a 1080x1920 render on a software GPU by quality, per shot showing (see `short.render`).
const SOFTWARE_FRAME_S: [f32; 3] = [0.6, 1.5, 2.0];
/// Seconds per frame that don't scale with the 3D: the performer, volumes, layers and the encoder.
const FRAME_OVERHEAD_S: f32 = 0.25;
/// Seconds to open a shot: building the character and settling it, on a software GPU.
const SHOT_SETUP_S: f32 = 20.0;

/// Where sequences live in the project.
fn sequence_path(id: &str) -> Result<DocPath, OpError> {
    DocPath::parse(&format!("sequences/{id}")).map_err(|e| OpError::invalid("id", e.to_string()))
}

fn checked_id(id: &str) -> Result<(), OpError> {
    Slug::new(id).map(|_| ()).map_err(|e| OpError::invalid("id", format!("'{id}' is not a sequence id: {e}")))
}

/// The patch that sets `path` to `value`, creating the `sequences` table in the same step when it is the first.
fn put(doc: &dyn DocRead, path: &DocPath, value: Value) -> Patch {
    let mut at = path.clone();
    let mut value = value;
    while let Some((parent, key)) = at.split_last() {
        if parent.segments().is_empty() || doc.get(&parent).is_some() {
            break;
        }
        let mut table = serde_json::Map::new();
        table.insert(key.to_owned(), value);
        value = Value::Object(table);
        at = parent;
    }
    Patch::new().with(at.clone(), doc.get(&at).cloned(), Some(value))
}

/// The sequence stored as `id`.
fn stored(doc: &dyn DocRead, id: &str) -> Result<SequenceDef, OpError> {
    checked_id(id)?;
    let node = doc.get(&sequence_path(id)?).ok_or_else(|| {
        OpError::not_found(format!("there is no sequence '{id}'")).hint("sequence.list lists them; sequence.set makes one")
    })?;
    serde_json::from_value(node.clone())
        .map_err(|e| OpError::invalid("id", format!("the sequence '{id}' doesn't read: {e}")))
}

/// Checks `def` against the project: its rules, and the characters and scenes it names.
fn check_in(doc: &dyn DocRead, def: &SequenceDef) -> Result<(), OpError> {
    def.check()?;
    for shot in &def.shots {
        for actor in &shot.actors {
            if doc.get(&char_paths::character(&actor.character)).is_none() {
                return Err(OpError::not_found(format!(
                    "shot '{}': there is no character '{}'",
                    shot.id, actor.character
                ))
                .hint("check the id; characters live under characters/<id>"));
            }
        }
        if let Some(scene) = &shot.scene {
            let id = SceneId::new(scene).map_err(|e| OpError::invalid("scene", e.to_string()))?;
            scene_for(doc, Some(&id))?;
        }
        if let Some(rig) = &shot.light_rig
            && !LightRig::names().any(|known| known == rig)
        {
            let known: Vec<&str> = LightRig::names().collect();
            return Err(OpError::invalid("light_rig", format!("shot '{}': there is no light rig '{rig}'", shot.id))
                .hint(format!("the light rigs are {}", known.join(", "))));
        }
    }
    Ok(())
}

/// Where a sequence comes from: the project's store, a file of the project, or inline.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SequenceArg {
    /// A sequence of the project, `sequences/<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A sequence file in the project (`sequences/weather.seq.toml`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The sequence itself, as the file's fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<Box<SequenceDef>>,
}

impl SequenceArg {
    /// The sequence, read and checked against the project.
    fn load(
        &self,
        doc: &dyn DocRead,
        fs: &dyn Fn(&str, FsMode) -> Result<omuzaki_ops_api::CanonicalPath, OpError>,
    ) -> Result<SequenceDef, OpError> {
        let def = match (&self.id, &self.path, &self.sequence) {
            (Some(id), None, None) => stored(doc, id)?,
            (None, Some(path), None) => {
                let file = fs(path, FsMode::Read)?.open_read()?;
                let mut text = String::new();
                file.take(MAX_FILE_BYTES + 1)
                    .read_to_string(&mut text)
                    .map_err(|e| OpError::invalid("path", format!("{path} doesn't read as text: {e}")))?;
                if text.len() as u64 > MAX_FILE_BYTES {
                    return Err(OpError::invalid("path", format!("{path} is over {MAX_FILE_BYTES} bytes")));
                }
                SequenceDef::from_toml(&text)?
            }
            (None, None, Some(def)) => (**def).clone(),
            _ => {
                return Err(OpError::invalid("sequence", "give exactly one of `id`, `path` and `sequence`")
                    .hint("`id` names a sequence of the project; `path` a *.seq.toml file; `sequence` is inline"));
            }
        };
        check_in(doc, &def)?;
        Ok(def)
    }

    /// The name outputs default to.
    fn stem(&self, def: &SequenceDef) -> String {
        if let Some(id) = &self.id {
            return id.clone();
        }
        if let Some(path) = &self.path {
            let name = path.rsplit('/').next().unwrap_or(path);
            return name.strip_suffix(".seq.toml").or_else(|| name.strip_suffix(".toml")).unwrap_or(name).to_owned();
        }
        let slug: String = def
            .title
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        if slug.is_empty() { "sequence".to_owned() } else { slug }
    }
}

fn example_sequence() -> Value {
    json!({
        "title": "Weather tease",
        "duration": 1.0,
        "fps": 12,
        "size": [270, 480],
        "shot": [{
            "id": "desk",
            "start": 0.0,
            "end": 1.0,
            "backdrop": { "kind": "gradient", "top": "#2a3a8f", "bottom": "#e05aa8" },
            "camera": { "position": [0.0, 1.4, 2.0], "target": [0.0, 1.3, 0.0], "fov": 30 },
            "actor": [{
                "id": "anchor",
                "character": "kit",
                "face": [{ "t": 0.0, "set": { "happy": 0.7 } }],
                "talk": [[0.1, 0.9]]
            }],
            "prop": [{ "id": "desk", "kind": "news-desk", "position": [0.0, 0.0, 0.45] }]
        }],
        "layer": [{
            "id": "title",
            "draw": { "kind": "text", "text": "WEATHER", "size": 64 },
            "y": 0.85,
            "x": [{ "t": 0.0, "v": -0.5 }, { "t": 0.4, "v": 0.5, "ease": "expo-out" }]
        }]
    })
}

/// Creates or replaces a whole sequence of the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"id": "weather", "sequence": example_sequence()}],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetSequence {
    /// The sequence's id, a slug.
    pub id: String,
    /// The sequence.
    pub sequence: Box<SequenceDef>,
}

impl Op for SetSequence {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.set",
            summary: "Create or replace a sequence of the project (`sequences/<id>`): keyframed shots (camera, one \
                      actor's pose, face, talking, gaze and surface fx such as a burn, props in the world or in a hand, \
                      toon volumes of smoke, goo or cloud) and motion-graphics layers (text, shapes, rings, bar charts, \
                      icons, speed lines) on one timeline, each value a track of eased keys. `sequence.kinds` lists \
                      what it may name; `sequence.render` makes the video.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        checked_id(&self.id)?;
        check_in(ctx.doc, &self.sequence)?;
        let value = serde_json::to_value(&*self.sequence)
            .map_err(|e| OpError::internal(format!("the sequence doesn't serialise: {e}")))?;
        let path = sequence_path(&self.id)?;
        let created = ctx.doc.get(&path).is_none();
        Ok(Plan {
            patch: put(ctx.doc, &path, value),
            output: Some(json!({
                "id": self.id,
                "created": created,
                "frames": self.sequence.frames(),
                "shots": self.sequence.shots.len(),
                "layers": self.sequence.layers.len(),
            })),
            ..Plan::default()
        })
    }
}

register_op!(SetSequence);

/// Sets or removes one key of a sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"id": "weather", "track": "shot/desk/camera/fov", "t": 0.8, "v": 22.0, "ease": "expo-out"},
        {"id": "weather", "track": "shot/desk/actor/anchor/pose", "t": 0.5, "v": {"pose": "point"}, "ease": "back-out"},
        {"id": "weather", "track": "layer/title/opacity", "t": 0.9, "v": 0.0},
        {"id": "weather", "track": "shot/desk/actor/anchor/face", "t": 0.0, "remove": true}
    ],
    "x-omuzaki-fixture" = "sequence"
))]
pub struct KeySequence {
    /// The sequence.
    pub id: String,
    /// The track, as a path through the sequence's fields: list items by their `id` or index
    /// (`shot/desk/camera/position`, `layer/title/x`, `shot/desk/actor/anchor/pose`, `shot/0/prop/card/attach/weight`).
    pub track: String,
    /// When, seconds (from the shot's start inside a shot).
    pub t: f32,
    /// The value: a number, a colour, a list of numbers; or, for a pose or face track, the key's fields
    /// (`{"pose": "point", "bones": {...}}`, `{"set": {"happy": 1}}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<Value>,
    /// How the track travels into this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ease: Option<Ease>,
    /// Remove the key at `t` instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remove: bool,
}

/// The time two keys share when they are this close, seconds.
const SAME_TIME: f64 = 1e-4;

/// Walks `root` along `segments`, list items by `id` or index; creates nothing.
fn walk<'a>(root: &'a mut Value, segments: &[&str]) -> Result<&'a mut Value, String> {
    let mut at = root;
    for (i, segment) in segments.iter().enumerate() {
        let here = segments[..i].join("/");
        at = match at {
            Value::Object(map) => {
                let last = i + 1 == segments.len();
                if !map.contains_key(*segment) && last {
                    map.insert((*segment).to_owned(), Value::Null);
                }
                map.get_mut(*segment).ok_or_else(|| format!("'{here}' has no field '{segment}'"))?
            }
            Value::Array(items) => {
                let index = segment.parse::<usize>().ok().or_else(|| {
                    items.iter().position(|item| item.get("id").and_then(Value::as_str) == Some(*segment))
                });
                let index = index.ok_or_else(|| format!("'{here}' has no item '{segment}'"))?;
                items.get_mut(index).ok_or_else(|| format!("'{here}' has no item {index}"))?
            }
            _ => return Err(format!("'{here}' is a value, not a table or a list")),
        };
    }
    Ok(at)
}

fn key_time(key: &Value) -> Option<f64> {
    key.get("t").and_then(Value::as_f64)
}

/// Whether `v` is a list of keys (objects with a `t`), not a held value.
fn is_keys(v: &Value) -> bool {
    v.as_array().is_some_and(|items| !items.is_empty() && items.iter().all(|k| k.is_object() && key_time(k).is_some()))
}

impl KeySequence {
    /// The sequence with the key set or removed.
    fn keyed(&self, def: &SequenceDef) -> Result<SequenceDef, OpError> {
        if !self.t.is_finite() {
            return Err(OpError::invalid("t", "a key's time is a number of seconds"));
        }
        let mut root = serde_json::to_value(def).map_err(|e| OpError::internal(e.to_string()))?;
        let segments: Vec<&str> = self.track.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return Err(OpError::invalid("track", "name a track: `shot/<id>/camera/fov`, `layer/<id>/x` ..."));
        }
        let track = walk(&mut root, &segments).map_err(|e| OpError::invalid("track", e))?;
        let t = f64::from(self.t);
        let mut keys = if is_keys(track) {
            track.as_array().cloned().unwrap_or_default()
        } else if track.is_null() || track.as_array().is_some_and(Vec::is_empty) {
            Vec::new()
        } else if self.v.as_ref().is_some_and(Value::is_object) && track.is_array() {
            Vec::new()
        } else {
            // A held value becomes a key at 0, so the new key travels from it.
            let held = track.clone();
            vec![json!({ "t": 0.0, "v": held })]
        };
        let found = keys.iter().position(|k| key_time(k).is_some_and(|kt| (kt - t).abs() < SAME_TIME));
        if self.remove {
            let Some(at) = found else {
                return Err(OpError::not_found(format!("'{}' has no key at {} s", self.track, self.t)));
            };
            keys.remove(at);
        } else {
            let Some(v) = &self.v else {
                return Err(OpError::invalid("v", "give the key's value `v`, or `remove: true`"));
            };
            let mut key = match v {
                Value::Object(fields) if !fields.contains_key("v") && !is_value_object(fields) => {
                    let mut fields = fields.clone();
                    fields.insert("t".into(), json!(self.t));
                    Value::Object(fields)
                }
                _ => json!({ "t": self.t, "v": v }),
            };
            if let (Some(ease), Value::Object(fields)) = (&self.ease, &mut key) {
                fields.insert("ease".into(), serde_json::to_value(ease).map_err(|e| OpError::internal(e.to_string()))?);
            }
            match found {
                Some(at) => keys[at] = key,
                None => {
                    let at = keys.partition_point(|k| key_time(k).is_some_and(|kt| kt < t));
                    keys.insert(at, key);
                }
            }
        }
        *track = Value::Array(keys);
        let keyed: SequenceDef = serde_json::from_value(root)
            .map_err(|e| OpError::invalid("v", format!("the track '{}' doesn't take that key: {e}", self.track)))?;
        Ok(keyed)
    }
}

/// Whether `fields` is a value of a track (a colour as `{r, g, b}` and the like) rather than a key's fields.
fn is_value_object(fields: &serde_json::Map<String, Value>) -> bool {
    fields.contains_key("r") && fields.contains_key("g") && fields.contains_key("b")
}

impl Op for KeySequence {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.key",
            summary: "Set or remove one key of a sequence's track by its path (`shot/desk/camera/fov`, `layer/title/x`, \
                      `shot/desk/actor/anchor/pose`), with its ease (`expo-out`, `back-out`, a bezier, a spring). A \
                      held value becomes keys; a pose or face key takes its fields as `v`.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = stored(ctx.doc, &self.id)?;
        let keyed = self.keyed(&def)?;
        check_in(ctx.doc, &keyed)?;
        let value = serde_json::to_value(&keyed).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan {
            patch: put(ctx.doc, &sequence_path(&self.id)?, value),
            output: Some(json!({ "id": self.id, "track": self.track, "t": self.t, "removed": self.remove })),
            ..Plan::default()
        })
    }
}

register_op!(KeySequence);

/// Deletes a sequence of the project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"id": "weather"}], "x-omuzaki-fixture" = "sequence"))]
pub struct DeleteSequence {
    /// The sequence.
    pub id: String,
}

impl Op for DeleteSequence {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.delete",
            summary: "Delete a sequence of the project (its rendered videos stay).",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        stored(ctx.doc, &self.id)?;
        let path = sequence_path(&self.id)?;
        Ok(Plan {
            patch: Patch::new().with(path.clone(), ctx.doc.get(&path).cloned(), None),
            output: Some(json!({ "id": self.id, "deleted": true })),
            ..Plan::default()
        })
    }
}

register_op!(DeleteSequence);

/// Reads a sequence of the project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"id": "weather"}], "x-omuzaki-fixture" = "sequence"))]
pub struct GetSequence {
    /// The sequence.
    pub id: String,
}

impl Op for GetSequence {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.get",
            summary: "Read a sequence of the project: every shot, key and layer, as `sequence.set` takes it.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = stored(ctx.doc, &self.id)?;
        Ok(Plan::answer(json!({ "id": self.id, "sequence": def })))
    }
}

register_op!(GetSequence);

/// Lists the sequences of the project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}], "x-omuzaki-fixture" = "sequence"))]
pub struct ListSequences {}

impl Op for ListSequences {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.list",
            summary: "List the sequences of the project: id, title, length, frame rate, size, shots and layers.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let root = DocPath::parse("sequences").map_err(|e| OpError::internal(e.to_string()))?;
        let mut out = Vec::new();
        if let Some(Value::Object(map)) = ctx.doc.get(&root) {
            for (id, node) in map {
                let row = match serde_json::from_value::<SequenceDef>(node.clone()) {
                    Ok(def) => json!({
                        "id": id, "title": def.title, "duration": def.duration, "fps": def.fps, "size": def.size,
                        "shots": def.shots.len(), "layers": def.layers.len(),
                    }),
                    Err(e) => json!({ "id": id, "unreadable": e.to_string() }),
                };
                out.push(row);
            }
        }
        Ok(Plan::answer(json!({ "sequences": out })))
    }
}

register_op!(ListSequences);

/// Checks a sequence and says what it holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{"source": {"id": "weather"}}, {"source": {"sequence": example_sequence()}}],
    "x-omuzaki-fixture" = "sequence"
))]
pub struct CheckSequence {
    /// The sequence.
    pub source: SequenceArg,
}

impl Op for CheckSequence {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.check",
            summary: "Check a sequence (of the project, a file or inline) as a render would, and say what it holds: its \
                      frames, every shot's span, actor, props and volumes, its layers, and the stretches no shot or \
                      back layer covers.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let fs = |path: &str, mode| ctx.fs_access(path, mode);
        let def = self.source.load(ctx.doc, &fs)?;
        let shots: Vec<Value> = def
            .shots
            .iter()
            .map(|s| {
                json!({
                    "id": s.id, "start": s.start, "end": s.end,
                    "actor": s.actors.first().map(|a| json!({ "id": a.id, "character": a.character.as_str(),
                        "pose_keys": a.pose.len(), "face_keys": a.face.len(), "talk": a.talk, "surface": a.surface.is_some() })),
                    "props": s.props.iter().map(|p| json!({ "id": p.id, "kind": p.kind, "held": p.attach.is_some() })).collect::<Vec<_>>(),
                    "volumes": s.volumes.iter().map(|v| json!({ "id": v.id, "preset": v.preset.name() })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let layers: Vec<Value> =
            def.layers.iter().map(|l| json!({ "id": l.id, "kind": l.draw.kind(), "depth": l.depth, "start": l.start, "end": l.end })).collect();
        Ok(Plan::answer(json!({
            "ok": true,
            "duration": def.duration,
            "fps": def.fps,
            "frames": def.frames(),
            "size": def.size,
            "shots": shots,
            "layers": layers,
            "uncovered": uncovered(&def),
        })))
    }
}

register_op!(CheckSequence);

/// The stretches of `def` (seconds) that no shot and no back layer covers: frames that would be empty.
fn uncovered(def: &SequenceDef) -> Vec<[f32; 2]> {
    let mut spans: Vec<[f32; 2]> = def.shots.iter().map(|s| [s.start, s.end]).collect();
    spans.extend(def.layers.iter().filter(|l| l.depth == crate::layer::Depth::Back).map(|l| [l.start, l.end.unwrap_or(def.duration)]));
    spans.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut out = Vec::new();
    let mut reached = 0.0f32;
    for [a, b] in spans {
        if a > reached + 1e-3 {
            out.push([reached, a.min(def.duration)]);
        }
        reached = reached.max(b);
    }
    if reached + 1e-3 < def.duration {
        out.push([reached, def.duration]);
    }
    out
}

/// Lists what a sequence may name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct SequenceKinds {}

impl Op for SequenceKinds {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.kinds",
            summary: "List what a sequence may name: the easings, layer kinds and icons, volume presets, prop kinds, \
                      library poses, hand shapes and expressions.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let poses: Vec<Value> = PoseLibrary::builtin()
            .map(|lib| lib.iter().map(|p| json!({ "id": p.id, "summary": p.summary })).collect())
            .unwrap_or_default();
        let hands: Vec<String> =
            HandLibrary::builtin().map(|lib| lib.iter().map(|h| h.id.to_string()).collect()).unwrap_or_default();
        let expressions: Vec<String> =
            ExpressionSet::library().map(|set| set.0.keys().map(ToString::to_string).collect()).unwrap_or_default();
        let props: Vec<Value> =
            omuzaki_stage_api::prop_kinds().iter().map(|k| json!({ "id": k.id, "summary": k.summary })).collect();
        Ok(Plan::answer(json!({
            "easings": Curve::ALL.iter().map(|c| c.name()).collect::<Vec<_>>(),
            "ease_forms": ["a named easing", "{\"bezier\": [x1, y1, x2, y2]}", "{\"spring\": {\"frequency\": 3, \"damping\": 0.4}}"],
            "layer_kinds": crate::layer::Draw::KINDS,
            "icons": ICONS,
            "volume_presets": VolumePreset::ALL.iter().map(|p| p.name()).collect::<Vec<_>>(),
            "surface_fx": ["tint", "dissolve"],
            "backdrops": ["none", "colour", "gradient", "scene"],
            "prop_kinds": props,
            "poses": poses,
            "hand_shapes": hands,
            "expressions": expressions,
        })))
    }
}

register_op!(SequenceKinds);

/// The scene a shot of `def` stands in, with its actor cast alone.
fn shot_scene(
    doc: &dyn DocRead,
    shot: &Shot,
    character: &CharId,
) -> Result<(omuzaki_stage_api::SceneDef, omuzaki_stage_api::MemberId), OpError> {
    let scene = shot.scene.as_ref().map(SceneId::new).transpose().map_err(|e| OpError::invalid("scene", e.to_string()))?;
    let (_, mut scene) = scene_for(doc, scene.as_ref())?;
    if let Some(name) = &shot.light_rig {
        scene.lighting = LightRig::load(name)
            .and_then(|rig| rig.to_lighting())
            .map_err(|e| OpError::internal(format!("the light rig '{name}' doesn't load: {e}")))?;
    }
    let member = cast_alone(&mut scene, character, None)
        .map_err(|e| OpError::internal(format!("casting the character: {e}")))?;
    Ok((scene, member))
}

/// Loads every shot's body and scene from the project a job reads.
fn shot_loader<'a>(
    job: &'a JobCtx<'a>,
    def: &'a SequenceDef,
    quality: RenderQuality,
) -> impl FnMut(&Shot) -> Result<ShotStage, SequenceError> + 'a {
    move |shot: &Shot| {
        let fail = |e: OpError| SequenceError::Render(format!("shot '{}': {}", shot.id, e.message));
        let actor = shot.actors.first().ok_or_else(|| SequenceError::Invalid(format!("shot '{}' has no actor", shot.id)))?;
        let (scene, member) = job.read(|doc| shot_scene(doc, shot, &actor.character)).map_err(fail)?;
        let clock = SystemClock::default();
        let services = BodyServices::new(
            job.service::<dyn CharacterBuilder>().map_err(fail)?,
            job.service::<BuildServices>().map_err(fail)?,
            job.service::<dyn Store>().map_err(fail)?,
            job.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon),
            &clock,
        );
        let body: Body =
            job.read(|doc| load_body(doc, &services, &actor.character, None, quality.detail())).map_err(fail)?;
        let landmarks = body.landmarks().map_err(|e| SequenceError::Render(e.to_string()))?;
        let [w, h] = def.size;
        #[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
        let aspect = w as f32 / h as f32;
        let mut staged: StagedScene =
            stage_alone(&scene, &member, landmarks, aspect).map_err(|e| SequenceError::Render(e.to_string()))?;
        if staged.resolved.backdrop_image.is_some() {
            let store = job.service::<dyn Store>().map_err(fail)?;
            staged.resolved.load_backdrop(store).map_err(|e| SequenceError::Render(e.to_string()))?;
        }
        if staged.resolved.environment.and_then(|e| e.hdri()).is_some() {
            let store = job.service::<dyn Store>().map_err(fail)?;
            staged.resolved.load_hdri(store).map_err(|e| SequenceError::Render(e.to_string()))?;
        }
        if let Body::Kit(built) = &body {
            post_in_look(&mut staged.resolved.post, &built.look().scene);
            occlusion_in_look(&mut staged.resolved.occlusion, &built.look().scene);
        }
        Ok(ShotStage { body, staged })
    }
}

/// The estimate of a render of `frames` of `def` at `quality`.
#[allow(clippy::cast_precision_loss, reason = "frame and pixel counts")]
fn render_seconds(def: &SequenceDef, frames: &[u64], quality: RenderQuality) -> f32 {
    let share = (def.size[0] as f32 * def.size[1] as f32) / (1080.0 * 1920.0);
    let per_frame = match quality {
        RenderQuality::Live => SOFTWARE_FRAME_S[0],
        RenderQuality::High => SOFTWARE_FRAME_S[1],
        RenderQuality::Offline => SOFTWARE_FRAME_S[2],
    };
    let shown: f32 = frames
        .iter()
        .map(|i| {
            let t = *i as f32 / def.fps as f32;
            def.shots.iter().filter(|s| s.start <= t && t < s.end).count() as f32
        })
        .sum();
    SHOT_SETUP_S * def.shots.len() as f32 + shown * per_frame * share + frames.len() as f32 * FRAME_OVERHEAD_S
}

/// The plan every render job shares: the outputs it writes and its cost.
fn render_plan(ctx: &OpCtx<'_>, def: &SequenceDef, out: &str, frames: &[u64], quality: RenderQuality) -> Result<Plan, OpError> {
    ctx.fs_access(out, FsMode::Write)?;
    let size = check_size(def.size, quality)?;
    let mut character = 0;
    for shot in &def.shots {
        for actor in &shot.actors {
            character = character.max(character_bytes(ctx, &actor.character, None, quality)?);
        }
    }
    // Two shots alive at once (a crossfade) hold two characters.
    let gpu = cost(size, 2, quality, character.saturating_mul(2));
    Ok(Plan { cost: Some(CostHint { seconds: Some(render_seconds(def, frames, quality)), ..gpu }), ..Plan::default() })
}

/// Writes `bytes` at `path` in the project, replacing what is there.
fn write_file(job: &JobCtx<'_>, path: &str, bytes: &[u8]) -> Result<(), OpError> {
    use std::io::Write as _;
    let target = job.fs_access(path, FsMode::Write)?;
    if let Some(folder) = target.parent() {
        folder.create_dir_all()?;
    }
    let mut replacement = target.replace()?;
    replacement.write_all(bytes).map_err(|e| OpError::invalid("out", format!("couldn't write {path}: {e}")))?;
    replacement.commit()
}

fn renderer_of<'a>(job: &'a JobCtx<'_>) -> Result<std::sync::MutexGuard<'a, Box<dyn Renderer>>, OpError> {
    let renderer = job.service::<Mutex<Box<dyn Renderer>>>()?;
    renderer.lock().map_err(|_| OpError::internal("the renderer is unusable: an earlier render panicked while holding it"))
}

/// Renders a sequence to a video.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"source": {"id": "weather"}},
        {"source": {"sequence": example_sequence()}, "out": "renders/tease.webm", "quality": "live"}
    ],
    "x-omuzaki-fixture" = "sequence"
))]
pub struct RenderSequence {
    /// The sequence.
    pub source: SequenceArg,
    /// The video, a `.webm` in the project; `renders/<sequence>.webm` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    /// The render settings.
    #[serde(default)]
    pub quality: RenderQuality,
    /// Only this stretch, `[from, to]` seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f32; 2]>,
    /// Replace a file already at `out`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// The frames of `def` in `range` seconds.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "at most 600 s at 60 fps")]
fn frames_in(def: &SequenceDef, range: Option<[f32; 2]>) -> Result<Vec<u64>, OpError> {
    let all = def.frames();
    let Some([from, to]) = range else { return Ok((0..all).collect()) };
    if !(from.is_finite() && to.is_finite() && from >= 0.0 && to > from) {
        return Err(OpError::invalid("range", "a range is [from, to] seconds with to after from"));
    }
    let first = (f64::from(from) * f64::from(def.fps)).floor() as u64;
    let last = ((f64::from(to) * f64::from(def.fps)).ceil() as u64).min(all);
    if first >= last {
        return Err(OpError::invalid("range", format!("the sequence is {} s long", def.duration)));
    }
    Ok((first..last).collect())
}

impl RenderSequence {
    fn out(&self, def: &SequenceDef) -> String {
        self.out.clone().unwrap_or_else(|| format!("renders/{}.webm", self.source.stem(def)))
    }
}

impl Op for RenderSequence {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.render",
            summary: "Render a sequence (of the project as `id`, a `*.seq.toml` file as `path`, or inline) to a WebM \
                      video in the project: each shot staged as it shows (its actor performing its keys with life, its \
                      props, toon volumes and surface fx), its camera keyed, overlapping shots crossfaded, and the \
                      motion-graphics layers drawn behind and in front. Frame-exact and headless.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let fs = |path: &str, mode| ctx.fs_access(path, mode);
        let def = self.source.load(ctx.doc, &fs)?;
        let frames = frames_in(&def, self.range)?;
        render_plan(ctx, &def, &self.out(&def), &frames, self.quality)
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let started = std::time::Instant::now();
        let fs = |path: &str, mode| job.fs_access(path, mode);
        let def = job.read(|doc| self.source.load(doc, &fs))?;
        let out = self.out(&def);
        let frames = frames_in(&def, self.range)?;
        let target = job.fs_access(&out, FsMode::Write)?;
        if let Some(folder) = target.parent() {
            folder.create_dir_all()?;
        }
        if !self.replace && target.open_read().is_ok() {
            return Err(OpError::invalid("out", format!("{out} is already there")).hint("pass `replace: true`, or another `out`"));
        }
        job.checkpoint("loading", 0.0)?;
        let mut replacement = target.replace()?;
        let file = replacement.file().try_clone().map_err(|e| OpError::internal(format!("the video file's handle: {e}")))?;
        let mut writer = ShortWriter::create(file, std::path::Path::new(&out), def.size, def.fps, None).map_err(SequenceError::from)?;
        let mut stopped: Option<OpError> = None;
        let mut done = 0usize;
        let mut progress = |share: f32| {
            done += 1;
            if done % PROGRESS_EVERY != 0 {
                return Ok(());
            }
            job.checkpoint("rendering", 0.02 + 0.95 * share).map_err(|e| {
                stopped = Some(e);
                SequenceError::Render("stopped".into())
            })
        };
        let mut load = shot_loader(job, &def, self.quality);
        let outcome = {
            let mut renderer = renderer_of(job)?;
            render_frames(&mut **renderer, &def, self.quality.quality(), &mut load, &frames, &mut writer, &mut progress)
        };
        if let Some(e) = stopped {
            return Err(e);
        }
        let report = outcome?;
        job.checkpoint("finishing", 0.98)?;
        let written = writer.finish().map_err(SequenceError::from)?;
        replacement.commit()?;
        let elapsed = started.elapsed().as_secs_f32();
        #[allow(clippy::cast_precision_loss, reason = "frame counts")]
        let seconds = frames.len() as f32 / def.fps as f32;
        tracing::info!(frames = report.frames, render_s = report.render_s, composite_s = report.composite_s, "sequence rendered");
        job.checkpoint("done", 1.0)?;
        Ok(json!({
            "path": out,
            "bytes": written.bytes,
            "frames": report.frames,
            "fps": def.fps,
            "size": def.size,
            "duration_s": seconds,
            "shots_opened": report.shots_opened,
            "render_s": report.render_s,
            "composite_s": report.composite_s,
            "realtime_factor": elapsed / seconds.max(0.001),
        }))
    }
}

register_op!(RenderSequence);

/// Renders frames of a sequence at chosen times as one PNG sheet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"source": {"id": "weather"}, "times": [0.0, 0.5, 0.9]},
        {"source": {"id": "weather"}, "times": [0.4], "out": "renders/weather-title.png", "scale": 1.0}
    ],
    "x-omuzaki-fixture" = "sequence"
))]
pub struct SequenceStills {
    /// The sequence.
    pub source: SequenceArg,
    /// The times, seconds, increasing; 1 to 48.
    pub times: Vec<f32>,
    /// The sheet, a `.png` in the project; `renders/<sequence>-stills.png` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    /// Each frame's size on the sheet against the sequence's, 0.05 to 1; 0.25 by default.
    #[serde(default = "quarter")]
    pub scale: f32,
    /// Frames across; as many as fit in 6 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
    /// The render settings.
    #[serde(default)]
    pub quality: RenderQuality,
}

fn quarter() -> f32 {
    0.25
}

impl SequenceStills {
    fn out(&self, def: &SequenceDef) -> String {
        self.out.clone().unwrap_or_else(|| format!("renders/{}-stills.png", self.source.stem(def)))
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "at most 600 s at 60 fps")]
    fn frames(&self, def: &SequenceDef) -> Result<Vec<u64>, OpError> {
        if self.times.is_empty() || self.times.len() > MAX_STILLS {
            return Err(OpError::invalid("times", format!("1 to {MAX_STILLS} times")));
        }
        if !(0.05..=1.0).contains(&self.scale) {
            return Err(OpError::invalid("scale", "0.05 to 1"));
        }
        let last = def.frames() - 1;
        let frames: Vec<u64> = self
            .times
            .iter()
            .map(|t| ((f64::from(t.max(0.0)) * f64::from(def.fps)).round() as u64).min(last))
            .collect();
        if frames.windows(2).any(|w| w[1] <= w[0]) {
            return Err(OpError::invalid("times", "times go up, at least a frame apart"));
        }
        Ok(frames)
    }
}

impl Op for SequenceStills {
    fn meta() -> OpMeta {
        OpMeta {
            name: "sequence.stills",
            summary: "Render frames of a sequence at chosen times into one PNG sheet in the project (one time: a still \
                      at `scale` 1), to look at keys, poses and layers without rendering the video.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let fs = |path: &str, mode| ctx.fs_access(path, mode);
        let def = self.source.load(ctx.doc, &fs)?;
        let frames = self.frames(&def)?;
        render_plan(ctx, &def, &self.out(&def), &frames, self.quality)
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let fs = |path: &str, mode| job.fs_access(path, mode);
        let def = job.read(|doc| self.source.load(doc, &fs))?;
        let frames = self.frames(&def)?;
        let out = self.out(&def);
        job.checkpoint("rendering", 0.0)?;
        let mut sink = Collect::default();
        let mut progress = |share: f32| job.checkpoint("rendering", 0.9 * share).map_err(|e| SequenceError::Render(e.message));
        let mut load = shot_loader(job, &def, self.quality);
        {
            let mut renderer = renderer_of(job)?;
            render_frames(&mut **renderer, &def, self.quality.quality(), &mut load, &frames, &mut sink, &mut progress)?;
        }
        let png = sheet(&sink.0, def.size, self.scale, self.columns)?;
        write_file(job, &out, &png)?;
        job.checkpoint("done", 1.0)?;
        Ok(json!({ "path": out, "frames": frames, "bytes": png.len() }))
    }
}

register_op!(SequenceStills);

/// The frames as one PNG: a grid `columns` across of each frame scaled by `scale`, on a dark ground.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss, reason = "pixel sizes")]
fn sheet(frames: &[(u64, Vec<u8>)], size: [u32; 2], scale: f32, columns: Option<u32>) -> Result<Vec<u8>, OpError> {
    let n = u32::try_from(frames.len()).unwrap_or(1).max(1);
    let columns = columns.unwrap_or(n.min(6)).clamp(1, n);
    let rows = n.div_ceil(columns);
    let (fw, fh) = (((size[0] as f32 * scale).round() as u32).max(1), ((size[1] as f32 * scale).round() as u32).max(1));
    let gap = if n > 1 { 8 } else { 0 };
    let (w, h) = (columns * fw + (columns + 1) * gap, rows * fh + (rows + 1) * gap);
    let mut canvas = image::RgbaImage::from_pixel(w, h, image::Rgba([24, 24, 28, 255]));
    for (i, (_, rgba)) in frames.iter().enumerate() {
        let frame = image::RgbaImage::from_raw(size[0], size[1], rgba.clone())
            .ok_or_else(|| OpError::internal("a frame is the wrong size"))?;
        let frame = if (scale - 1.0).abs() < 1e-6 {
            frame
        } else {
            image::imageops::resize(&frame, fw, fh, image::imageops::FilterType::Triangle)
        };
        let i = u32::try_from(i).unwrap_or(0);
        let (x, y) = (gap + (i % columns) * (fw + gap), gap + (i / columns) * (fh + gap));
        image::imageops::overlay(&mut canvas, &frame, i64::from(x), i64::from(y));
    }
    let mut png = Vec::new();
    canvas
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| OpError::internal(format!("encoding the sheet: {e}")))?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> SequenceDef {
        serde_json::from_value(example_sequence()).unwrap()
    }

    fn key(track: &str, t: f32, v: Value) -> KeySequence {
        KeySequence { id: "s".into(), track: track.into(), t, v: Some(v), ease: Some(Ease::Named(Curve::ExpoOut)), remove: false }
    }

    #[test]
    fn the_example_checks() {
        def().check().unwrap();
    }

    #[test]
    fn keying_a_held_value_makes_keys() {
        let keyed = key("shot/desk/camera/fov", 0.8, json!(22.0)).keyed(&def()).unwrap();
        let fov = &keyed.shots[0].camera.fov;
        assert!(fov.is_animated());
        assert!((fov.at(0.0) - 30.0).abs() < 1e-5, "it starts from the held value");
        assert!((fov.at(0.8) - 22.0).abs() < 1e-5);
    }

    #[test]
    fn keys_replace_at_the_same_time_and_remove() {
        let once = key("layer/title/x", 0.4, json!(0.25)).keyed(&def()).unwrap();
        let crate::curve::Track::Keys(keys) = &once.layers[0].x else { panic!("keys") };
        assert_eq!(keys.len(), 2, "the key at 0.4 is replaced, not added");
        let mut remove = key("layer/title/x", 0.4, json!(0));
        remove.remove = true;
        let removed = remove.keyed(&once).unwrap();
        let crate::curve::Track::Keys(keys) = &removed.layers[0].x else { panic!("keys") };
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn pose_keys_take_their_fields() {
        let keyed = key("shot/desk/actor/anchor/pose", 0.5, json!({ "pose": "point" })).keyed(&def()).unwrap();
        let pose = &keyed.shots[0].actors[0].pose;
        assert_eq!(pose.len(), 1);
        assert_eq!(pose[0].pose.as_deref(), Some("point"));
        let face = key("shot/desk/actor/anchor/face", 0.5, json!({ "set": { "shocked": 1.0 } })).keyed(&def()).unwrap();
        assert_eq!(face.shots[0].actors[0].face.len(), 2);
    }

    #[test]
    fn a_bad_track_names_where_it_went_wrong() {
        let err = key("shot/nope/camera/fov", 0.1, json!(1)).keyed(&def()).unwrap_err();
        assert!(err.message.contains("nope"), "{}", err.message);
    }

    #[test]
    fn uncovered_spans_are_found() {
        let mut d = def();
        d.duration = 1.5;
        assert_eq!(uncovered(&d), vec![[1.0, 1.5]]);
    }

    #[test]
    fn a_sheet_lays_frames_in_a_grid() {
        let frame = vec![255u8; 16 * 16 * 4];
        let frames = vec![(0, frame.clone()), (1, frame.clone()), (2, frame)];
        let png = sheet(&frames, [16, 16], 0.5, Some(2)).unwrap();
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (2 * 8 + 3 * 8, 2 * 8 + 3 * 8));
    }
}
