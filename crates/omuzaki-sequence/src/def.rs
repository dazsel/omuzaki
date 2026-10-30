//! The sequence: what a `*.seq.toml` file and the `sequences/<id>` node of a project hold.
//!
//! A sequence is a list of shots on one clock plus motion-graphics layers over the whole frame. A shot is a 3D scene for
//! a span of time: a camera, characters (actors) with their pose, face, talking and surface fx keyed, props (any
//! registered prop kind, in hands or in the world) and toon volumes (smoke, goo, cloud). Shots may overlap: their
//! `opacity` tracks crossfade them. Layers are 2D: text, shapes, charts and icons, drawn behind the 3D frame (`back`)
//! or over it (`front`), so a lower third, a stinger or a news set's logo wall is part of the same timeline.
//!
//! ```toml
//! duration = 3.0
//! fps = 24
//! size = [1080, 1920]
//!
//! [[shot]]
//! id = "desk"
//! start = 0.0
//! end = 3.0
//! camera = { position = [0.0, 1.35, 2.2], target = [0.0, 1.3, 0.0], fov = 28 }
//!
//! [[shot.actor]]
//! id = "anchor"
//! character = "kit"
//! pose = [{ t = 0.0, pose = "stand-relaxed" }, { t = 1.0, pose = "point", ease = "back-out" }]
//! face = [{ t = 0.0, set = { happy = 0.6 } }]
//! talk = [[0.2, 2.5]]
//!
//! [[layer]]
//! id = "title"
//! draw = { kind = "text", text = "WEATHER" }
//! x = [{ t = 0.0, v = -0.5 }, { t = 0.4, v = 0.5, ease = "expo-out" }]
//! y = 0.3
//! ```

use std::collections::BTreeMap;

use omuzaki_char_api::CharId;
use omuzaki_core::colour::Colour;
use omuzaki_render_api::{SurfaceFx, SurfaceFxKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::curve::{Ease, Track};
use crate::error::SequenceError;
use crate::layer::Layer;
use crate::motion::MotionStyle;
use crate::volume::VolumeDef;

/// The longest sequence, seconds.
pub const MAX_DURATION_S: f32 = 600.0;
/// The most shots in a sequence.
pub const MAX_SHOTS: usize = 64;
/// The actors in one shot: one for now; whole casts in a shot come with the cast stage.
pub const MAX_ACTORS: usize = 1;
/// The most props in one shot.
pub const MAX_PROPS: usize = 32;
/// The most toon volumes in one shot.
pub const MAX_VOLUMES: usize = 8;
/// The most layers in a sequence.
pub const MAX_LAYERS: usize = 128;
/// The longest side of a sequence's frame, pixels.
pub const MAX_SIDE: u32 = 4096;

/// A whole sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SequenceDef {
    /// A title for people.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Seconds.
    pub duration: f32,
    /// Frames per second, 1 to 60.
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// Width and height in pixels; 9:16 1080x1920 by default.
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// Picks everything random (blinks, wiggles, the volumes' puffs).
    #[serde(default)]
    pub seed: u32,
    /// How the characters' keys play: a motion style preset (`smooth`, `anime-twos`, `cartoon-snappy`, `stop-motion`
    /// ...) and its knobs (`on`, `camera_on`, `snap`, `overshoot`, `boil`). An actor's own `motion` replaces it.
    #[serde(default, skip_serializing_if = "MotionStyle::is_default")]
    pub motion: MotionStyle,
    /// The shots, in any order; they may overlap (their `opacity` crossfades them).
    #[serde(default, rename = "shot", skip_serializing_if = "Vec::is_empty")]
    pub shots: Vec<Shot>,
    /// Motion-graphics layers over the whole frame, drawn in order (later on top).
    #[serde(default, rename = "layer", skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<Layer>,
}

fn default_fps() -> u32 {
    24
}

fn default_size() -> [u32; 2] {
    [1080, 1920]
}

fn yes() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
fn is_true(b: &bool) -> bool {
    *b
}

/// What is behind a shot's 3D frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Backdrop {
    /// Nothing: the layers behind (or the shot under it) show through. The default.
    #[default]
    None,
    /// One colour.
    Colour {
        /// The colour.
        colour: Colour,
    },
    /// A vertical gradient.
    Gradient {
        /// At the top edge.
        top: Colour,
        /// At the bottom edge.
        bottom: Colour,
    },
    /// The scene's own backdrop and sky.
    Scene,
}

/// A shot: a 3D scene over a span of the sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Shot {
    /// Its name, unique in the sequence.
    pub id: String,
    /// When it starts, seconds.
    pub start: f32,
    /// When it ends, seconds.
    pub end: f32,
    /// A scene of the project to stand it in (its light rig, ground and props); the project's default scene when left
    /// out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    /// A named light rig (`stage.light_rigs`) in place of the scene's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_rig: Option<String>,
    /// What is behind the 3D frame.
    #[serde(default)]
    pub backdrop: Backdrop,
    /// Hide the scene's ground disc and its own props, keeping only what the shot places. True by default.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub bare: bool,
    /// How much of the shot shows, 0 to 1: key it to crossfade with an overlapping shot.
    #[serde(default = "full", skip_serializing_if = "is_full")]
    pub opacity: Track<f32>,
    /// The camera.
    #[serde(default)]
    pub camera: CameraDef,
    /// The characters.
    #[serde(default, rename = "actor", skip_serializing_if = "Vec::is_empty")]
    pub actors: Vec<Actor>,
    /// Props of any registered kind (`stage.prop_kinds`), in the world or in a hand.
    #[serde(default, rename = "prop", skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<PropItem>,
    /// Toon volumes: cel-shaded smoke, goo, cloud.
    #[serde(default, rename = "volume", skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeDef>,
}

fn full() -> Track<f32> {
    Track::Hold(1.0)
}

fn is_full(t: &Track<f32>) -> bool {
    *t == full()
}

/// A shot's camera. Times are seconds from the shot's start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraDef {
    /// Where it is, metres.
    #[serde(default = "camera_position")]
    pub position: Track<[f32; 3]>,
    /// What it looks at, metres.
    #[serde(default = "camera_target")]
    pub target: Track<[f32; 3]>,
    /// The vertical field of view, degrees.
    #[serde(default = "camera_fov")]
    pub fov: Track<f32>,
    /// Roll about the view axis, degrees (a dutch angle).
    #[serde(default = "zero")]
    pub roll: Track<f32>,
    /// Handheld drift or an impact's shake on top of the keys.
    #[serde(default, skip_serializing_if = "Shake::is_still")]
    pub shake: Shake,
}

fn camera_position() -> Track<[f32; 3]> {
    Track::Hold([0.0, 1.35, 2.4])
}

fn camera_target() -> Track<[f32; 3]> {
    Track::Hold([0.0, 1.25, 0.0])
}

fn camera_fov() -> Track<f32> {
    Track::Hold(30.0)
}

fn zero() -> Track<f32> {
    Track::Hold(0.0)
}

impl Default for CameraDef {
    fn default() -> Self {
        Self {
            position: camera_position(),
            target: camera_target(),
            fov: camera_fov(),
            roll: zero(),
            shake: Shake::default(),
        }
    }
}

/// Noise on a camera: drift in position and turn, `amount` keyed so a shake can hit and fade.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Shake {
    /// How strong, 0 to 1 (1: about 2 cm and 1 degree).
    pub amount: Track<f32>,
    /// Wiggles a second.
    pub frequency: f32,
}

impl Default for Shake {
    fn default() -> Self {
        Self { amount: Track::Hold(0.0), frequency: 1.5 }
    }
}

impl Shake {
    fn is_still(&self) -> bool {
        self.amount == Track::Hold(0.0)
    }
}

/// A character in a shot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    /// Its name in the shot (props attach to it by this).
    pub id: String,
    /// The character of the project.
    pub character: CharId,
    /// Where its feet are, metres.
    #[serde(default = "origin")]
    pub position: Track<[f32; 3]>,
    /// Which way it faces, degrees about +Y (0 faces +Z, the default camera).
    #[serde(default = "zero")]
    pub yaw: Track<f32>,
    /// Pose keys: library poses and bone turns, blended key to key with their ease.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pose: Vec<PoseKey>,
    /// Face keys: expression weights, blended key to key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub face: Vec<FaceKey>,
    /// Where the eyes look: `[x, y]`, each -1 to 1 (right and up of straight ahead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gaze: Option<Track<[f32; 2]>>,
    /// When the mouth talks: `[start, end]` spans in seconds; the mouth shapes syllables through them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub talk: Vec<[f32; 2]>,
    /// Blinks and breathing on top of the keys. On by default.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub life: bool,
    /// A surface fx run over the body: a burn, a dissolve, turning to stone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceTrack>,
    /// This actor's motion style, in place of the sequence's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<MotionStyle>,
}

fn origin() -> Track<[f32; 3]> {
    Track::Hold([0.0; 3])
}

/// One pose key. Without `pose` the key starts from `stand-relaxed`; `bones` then set bones outright.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoseKey {
    /// When, seconds from the shot's start.
    pub t: f32,
    /// A pose of the library (`pose.list`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<String>,
    /// Reflect the library pose left to right.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror: bool,
    /// Bones set outright, by VRM name (`rightUpperArm`), as Euler XYZ degrees in the bone's parent space, the
    /// library's convention.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bones: BTreeMap<String, [f32; 3]>,
    /// Hand shapes (`fist`, `point`, `open`, ...) by side, `left` and `right`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hands: BTreeMap<String, String>,
    /// Where each hand goes, by side (`left`, `right`), in place of the library pose's: the arm bends (two-bone IK) so
    /// the wrist lands on a point riding on a bone of the same body, on any body (hands folded on a desk, a palm on
    /// the chest).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reach: BTreeMap<String, KeyReach>,
    /// How the body travels here from the key before.
    #[serde(default)]
    pub ease: Ease,
}

/// Where one hand goes in a pose key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyReach {
    /// The bone the target rides on, by VRM name (`hips`, `spine`, `chest`, `upperChest`, `head`).
    pub bone: String,
    /// The wrist's target from the bone's joint, in arm lengths: `+X` the character's left, `+Y` up, `+Z` forward,
    /// turned with the bone.
    pub at: [f32; 3],
    /// The side the elbow bends toward, in the same frame; down and a little behind when left out.
    #[serde(default = "elbow_down")]
    pub pole: [f32; 3],
    /// How much of the reach applies, 0 to 1.
    #[serde(default = "one")]
    pub weight: f32,
}

fn elbow_down() -> [f32; 3] {
    [0.0, -1.0, -0.3]
}

fn one() -> f32 {
    1.0
}

/// One face key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaceKey {
    /// When, seconds from the shot's start.
    pub t: f32,
    /// Expression weights, 0 to 1, by expression id (`happy`, `shocked`, `rage`, `menace` ...); those left out are 0.
    #[serde(default)]
    pub set: BTreeMap<String, f32>,
    /// How the face travels here from the key before.
    #[serde(default)]
    pub ease: Ease,
}

/// A surface fx on an actor, its front keyed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceTrack {
    /// What happens behind the front.
    #[serde(default)]
    pub kind: SurfaceFxKind,
    /// How far the front has come, 0 (untouched) to 1 + `edge_width` (everywhere).
    pub amount: Track<f32>,
    /// The colour behind the front.
    #[serde(default = "char_colour")]
    pub colour: Colour,
    /// The band at the front.
    #[serde(default = "ember")]
    pub edge: Colour,
    /// The band's width, 0 to 0.5.
    #[serde(default = "edge_width")]
    pub edge_width: f32,
    /// The band's own light, 0 to 8.
    #[serde(default = "glow")]
    pub glow: f32,
    /// Blob size, metres.
    #[serde(default = "blob")]
    pub size: f32,
    /// A direction the front climbs along in the body's space (`[0, 1, 0]`: from the feet up), and how much it decides
    /// the front against the blobs (0 to 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep: Option<([f32; 3], f32)>,
    /// Materials whose names contain any of these keep their own look (`["eye"]` keeps the eyes clear).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spare: Vec<String>,
    /// The change goes under the painted face instead of over it: a charred head keeps its eyes, brows and mouth.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub under_paint: bool,
}

fn char_colour() -> Colour {
    SurfaceFx::default().colour
}

fn ember() -> Colour {
    SurfaceFx::default().edge
}

fn edge_width() -> f32 {
    SurfaceFx::default().edge_width
}

fn glow() -> f32 {
    SurfaceFx::default().glow
}

fn blob() -> f32 {
    SurfaceFx::default().size
}

impl SurfaceTrack {
    /// The surface fx at `t`, seeded by `seed`; `None` while the front hasn't started.
    pub fn at(&self, t: f32, seed: u32) -> Option<SurfaceFx> {
        let amount = self.amount.at(t).clamp(0.0, omuzaki_render_api::MAX_SURFACE_FX_AMOUNT);
        if amount <= 0.0 {
            return None;
        }
        let (sweep, sweep_mix) = self.sweep.unwrap_or(([0.0; 3], 0.0));
        Some(SurfaceFx {
            kind: self.kind,
            amount,
            colour: self.colour,
            edge: self.edge,
            edge_width: self.edge_width,
            glow: self.glow,
            size: self.size,
            seed,
            sweep: sweep.into(),
            sweep_range: omuzaki_core::glam::Vec2::new(0.0, 1.8),
            sweep_mix,
            under_paint: self.under_paint,
        })
    }
}

/// Where a prop is held.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Attach {
    /// The actor holding it.
    pub actor: String,
    /// The humanoid bone it rides on (`rightHand`, `head`).
    pub bone: String,
    /// How much it is held, 0 to 1: key it down to let go (the prop then goes to its `world_*` place) or up to catch.
    #[serde(default = "full", skip_serializing_if = "is_full")]
    pub weight: Track<f32>,
}

/// A prop in a shot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropItem {
    /// Its name in the shot.
    pub id: String,
    /// The registered kind (`desk`, `news-desk`, `skyline`, `card` ...; `stage.prop_kinds`).
    pub kind: String,
    /// The kind's parameters; what is left out takes the kind's default.
    #[serde(default = "empty_object", skip_serializing_if = "is_empty_object")]
    pub params: Value,
    /// Held by an actor: `position` and `rotation` are then in the bone's frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<Attach>,
    /// Metres (in the bone's frame when attached).
    #[serde(default = "origin")]
    pub position: Track<[f32; 3]>,
    /// Yaw, pitch and roll, degrees.
    #[serde(default = "zero_turn")]
    pub rotation: Track<[f32; 3]>,
    /// Uniform scale.
    #[serde(default = "full", skip_serializing_if = "is_full")]
    pub scale: Track<f32>,
    /// Where it goes when let go (`attach.weight` below 1), metres in the world.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_position: Option<Track<[f32; 3]>>,
    /// How it turns when let go, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_rotation: Option<Track<[f32; 3]>>,
    /// Shown only from and to these times, seconds from the shot's start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<[f32; 2]>,
}

fn zero_turn() -> Track<[f32; 3]> {
    Track::Hold([0.0; 3])
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

fn is_empty_object(v: &Value) -> bool {
    v.as_object().is_some_and(serde_json::Map::is_empty)
}

impl SequenceDef {
    /// Reads a sequence file.
    ///
    /// # Errors
    /// [`SequenceError::Invalid`] naming the line for a file that doesn't read.
    pub fn from_toml(text: &str) -> Result<Self, SequenceError> {
        toml::from_str(text).map_err(|e| SequenceError::Invalid(format!("the sequence doesn't read: {e}")))
    }

    /// The sequence as a file.
    ///
    /// # Errors
    /// [`SequenceError::Invalid`] if a value has no TOML form.
    pub fn to_toml(&self) -> Result<String, SequenceError> {
        toml::to_string_pretty(self).map_err(|e| SequenceError::Invalid(format!("the sequence doesn't write: {e}")))
    }

    /// The number of frames.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "at most 600 s at 60 fps")]
    pub fn frames(&self) -> u64 {
        (f64::from(self.duration) * f64::from(self.fps)).ceil().max(1.0) as u64
    }

    /// The shots that show at `t` (start inclusive, end exclusive), in draw order (by start, then as listed).
    pub fn shots_at(&self, t: f32) -> Vec<&Shot> {
        let mut shots: Vec<&Shot> = self.shots.iter().filter(|s| s.start <= t && t < s.end).collect();
        shots.sort_by(|a, b| a.start.total_cmp(&b.start));
        shots
    }

    /// Checks everything a render would trip on, so a bad sequence is refused before it starts.
    ///
    /// # Errors
    /// [`SequenceError::Invalid`] naming the first bad field.
    pub fn check(&self) -> Result<(), SequenceError> {
        let bad = |m: String| Err(SequenceError::Invalid(m));
        if !(self.duration > 0.0 && self.duration <= MAX_DURATION_S) {
            return bad(format!("duration is over 0 and at most {MAX_DURATION_S} seconds"));
        }
        if !(1..=60).contains(&self.fps) {
            return bad("fps is 1 to 60".into());
        }
        if self.size.iter().any(|s| *s < 16 || *s > MAX_SIDE) || self.size.iter().any(|s| s % 2 == 1) {
            return bad(format!("size is two even numbers of pixels, 16 to {MAX_SIDE}"));
        }
        self.motion.check("motion").map_err(SequenceError::Invalid)?;
        if self.shots.len() > MAX_SHOTS {
            return bad(format!("at most {MAX_SHOTS} shots"));
        }
        if self.layers.len() > MAX_LAYERS {
            return bad(format!("at most {MAX_LAYERS} layers"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for (i, shot) in self.shots.iter().enumerate() {
            if !ids.insert(shot.id.as_str()) {
                return bad(format!("shot[{i}]: the id '{}' is used twice", shot.id));
            }
            shot.check(&format!("shot[{i}]")).map_err(SequenceError::Invalid)?;
        }
        let mut layer_ids = std::collections::BTreeSet::new();
        for (i, layer) in self.layers.iter().enumerate() {
            if !layer_ids.insert(layer.id.as_str()) {
                return bad(format!("layer[{i}]: the id '{}' is used twice", layer.id));
            }
            layer.check(&format!("layer[{i}]")).map_err(SequenceError::Invalid)?;
        }
        Ok(())
    }
}

impl Shot {
    fn check(&self, at: &str) -> Result<(), String> {
        if !(self.start.is_finite() && self.end.is_finite() && self.end > self.start && self.start >= 0.0) {
            return Err(format!("{at}: start and end are seconds with end after start"));
        }
        if self.actors.len() != MAX_ACTORS {
            return Err(format!("{at}: a shot holds {MAX_ACTORS} actor for now (more come with casts)"));
        }
        if self.props.len() > MAX_PROPS {
            return Err(format!("{at}: at most {MAX_PROPS} props"));
        }
        if self.volumes.len() > MAX_VOLUMES {
            return Err(format!("{at}: at most {MAX_VOLUMES} volumes"));
        }
        self.opacity.check(&format!("{at}.opacity"))?;
        let c = &self.camera;
        c.position.check(&format!("{at}.camera.position"))?;
        c.target.check(&format!("{at}.camera.target"))?;
        c.fov.check(&format!("{at}.camera.fov"))?;
        c.roll.check(&format!("{at}.camera.roll"))?;
        c.shake.amount.check(&format!("{at}.camera.shake.amount"))?;
        for (i, actor) in self.actors.iter().enumerate() {
            actor.check(&format!("{at}.actor[{i}]"))?;
        }
        for (i, prop) in self.props.iter().enumerate() {
            let field = format!("{at}.prop[{i}]");
            prop.position.check(&format!("{field}.position"))?;
            prop.rotation.check(&format!("{field}.rotation"))?;
            prop.scale.check(&format!("{field}.scale"))?;
            if let Some(attach) = &prop.attach {
                if !self.actors.iter().any(|a| a.id == attach.actor) {
                    return Err(format!("{field}.attach: there is no actor '{}' in the shot", attach.actor));
                }
                if omuzaki_anim_api::HumanoidBone::from_name(&attach.bone).is_none() {
                    return Err(format!(
                        "{field}.attach: '{}' is not a humanoid bone (rightHand, head ...)",
                        attach.bone
                    ));
                }
                attach.weight.check(&format!("{field}.attach.weight"))?;
            }
            omuzaki_stage_api::prop_kind(&prop.kind)
                .map_err(|e| format!("{field}.kind: {e}"))?
                .check(&prop.params)
                .map_err(|e| format!("{field}.params: {e}"))?;
        }
        for (i, volume) in self.volumes.iter().enumerate() {
            volume.check(&format!("{at}.volume[{i}]"), &self.actors)?;
        }
        Ok(())
    }
}

impl Actor {
    fn check(&self, at: &str) -> Result<(), String> {
        self.position.check(&format!("{at}.position"))?;
        self.yaw.check(&format!("{at}.yaw"))?;
        if let Some(motion) = &self.motion {
            motion.check(&format!("{at}.motion"))?;
        }
        for (i, w) in self.pose.windows(2).enumerate() {
            if w[1].t < w[0].t {
                return Err(format!("{at}.pose[{}]: keys go in time order", i + 1));
            }
        }
        for (i, key) in self.pose.iter().enumerate() {
            key.ease.check(&format!("{at}.pose[{i}].ease"))?;
            for bone in key.bones.keys() {
                if omuzaki_anim_api::HumanoidBone::from_name(bone).is_none() {
                    return Err(format!("{at}.pose[{i}].bones: '{bone}' is not a humanoid bone"));
                }
            }
            for side in key.hands.keys() {
                if side != "left" && side != "right" {
                    return Err(format!("{at}.pose[{i}].hands: the sides are left and right, not '{side}'"));
                }
            }
        }
        for (i, w) in self.face.windows(2).enumerate() {
            if w[1].t < w[0].t {
                return Err(format!("{at}.face[{}]: keys go in time order", i + 1));
            }
        }
        for (i, key) in self.face.iter().enumerate() {
            key.ease.check(&format!("{at}.face[{i}].ease"))?;
            if key.set.values().any(|w| !(0.0..=1.0).contains(w)) {
                return Err(format!("{at}.face[{i}]: weights are 0 to 1"));
            }
        }
        for (i, [a, b]) in self.talk.iter().enumerate() {
            if !(a.is_finite() && b.is_finite() && b > a) {
                return Err(format!("{at}.talk[{i}]: a span is [start, end] with end after start"));
            }
        }
        if let Some(gaze) = &self.gaze {
            gaze.check(&format!("{at}.gaze"))?;
        }
        if let Some(surface) = &self.surface {
            surface.amount.check(&format!("{at}.surface.amount"))?;
            let probe = SurfaceFx {
                kind: surface.kind,
                amount: 0.5,
                colour: surface.colour,
                edge: surface.edge,
                edge_width: surface.edge_width,
                glow: surface.glow,
                size: surface.size,
                ..SurfaceFx::default()
            };
            probe.validate().map_err(|e| format!("{at}.surface: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two overlapping shots with a moving camera, a talking actor, a prop, a smoke volume, and layers behind and in
    /// front.
    const TWO_SHOTS: &str = r##"
title = "Two shots"
duration = 3.0
fps = 24
size = [540, 960]

[[layer]]
id = "wall"
depth = "back"
draw = { kind = "rect", w = 600, h = 1000 }
fill = "#22305a"

[[layer]]
id = "title"
draw = { kind = "text", text = "HELLO", size = 72 }
y = 0.8
x = [{ t = 0.0, v = -0.5 }, { t = 0.5, v = 0.5, ease = "back-out" }]

[[shot]]
id = "near"
start = 0.0
end = 1.75
camera = { position = [{ t = 0.0, v = [0.0, 1.4, 2.4] }, { t = 1.75, v = [0.0, 1.4, 2.0] }], target = [0.0, 1.3, 0.0], fov = 30 }

[[shot.actor]]
id = "host"
character = "kit"
talk = [[0.2, 1.4]]
face = [{ t = 0.0, set = { happy = 0.6 } }]

[[shot.prop]]
id = "desk"
kind = "news-desk"
position = [0.0, 0.0, 0.5]

[[shot]]
id = "far"
start = 1.5
end = 3.0
backdrop = { kind = "gradient", top = "#8fb4ff", bottom = "#f0f4ff" }
camera = { position = [0.0, 1.2, 4.5], target = [0.0, 1.0, 0.0], fov = 34 }

[[shot.actor]]
id = "host"
character = "kit"
pose = [{ t = 0.0, pose = "cheer" }]

[[shot.volume]]
id = "puff"
preset = "smoke"
emitter = { at = [0.0, 0.2, -0.5], radius = 0.3 }
"##;

    #[test]
    fn a_two_shot_sequence_reads_checks_and_round_trips() {
        let def = SequenceDef::from_toml(TWO_SHOTS).unwrap();
        def.check().unwrap();
        assert_eq!(def.frames(), 72);
        assert_eq!(def.shots.len(), 2);
        let again = SequenceDef::from_toml(&def.to_toml().unwrap()).unwrap();
        assert_eq!(again, def);
        let json: SequenceDef = serde_json::from_value(serde_json::to_value(&def).unwrap()).unwrap();
        assert_eq!(json, def);
    }

    #[test]
    fn a_shot_without_its_actor_is_refused() {
        let mut def = SequenceDef::from_toml(TWO_SHOTS).unwrap();
        def.shots[0].actors.clear();
        assert!(def.check().unwrap_err().to_string().contains("actor"));
    }
}
