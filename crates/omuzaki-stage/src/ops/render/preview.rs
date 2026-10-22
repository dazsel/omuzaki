//! `render.preview`: one picture of a character in a scene, stored as a PNG (spec v3 §11, design scenes D9). It is how
//! an agent sees what it made: any character of the project, built from the kit or imported, renders through the same
//! call, so a person and an agent look at the same pixels. The character stands alone in a copy of the scene (the
//! project's opening scene, or the built-in default), lit, framed and backed as that scene is; the document never
//! changes.
//!
//! A named render preset (`preset`, design likeness-measures D10) fixes part of the picture from library data
//! (`library/render-presets/<name>.toml`): `measure` shoots the face front-on through an orthographic camera under the
//! flat `measure-flat` rig, with the neutral face, no post effects and a dark line at least 1.5 px wide, so the
//! `measure.*` ops read the character's shape and nothing else. The result reports where the face layout's eyes, mouth
//! and chin landed in the picture (`face_landmarks`), which an agent passes to those ops as `source = "render"`.

use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, VariantId};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::DocRead;
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Camera, Capabilities, MtoonSupport, Outline, Post, Projection};
use omuzaki_stage_api::{CameraId, Framing, Lighting, MemberId, SceneDef, SceneError, SceneId};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::job::{RenderQuality, character_bytes, check_expression, check_size, cost, store_png, with_renderer};
use super::pose::PoseArg;
use super::still::{Shot, StillSpec, encode_png, render_body_stills};
use crate::body::{Body, BodyServices, load_body};
use crate::frame::face::FacePoints;
use crate::recipe::preset::{PresetOutline, PresetPost, PresetProjection};
use crate::recipe::{LightRig, RenderPreset};
use crate::scene::alone::{ShotChoice, StagedScene, cast_alone, post_in_look, stage_alone};
use crate::scene::default::scene_for;
use crate::scene::landmarks::Landmarks;

/// The size of a preview when none is given: a portrait frame.
const DEFAULT_SIZE: [u32; 2] = [768, 1024];

fn default_size() -> [u32; 2] {
    DEFAULT_SIZE
}

fn one() -> f32 {
    1.0
}

/// Renders one picture of a character in a scene and stores it as a PNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "framing": "bust", "yaw": 30.0, "size": [512, 512], "quality": "live"},
        {"character": "kit", "light_rig": "viewer-candle", "framing": "face"},
        {"character": "kit", "preset": "measure", "size": [768, 768]},
        {"character": "kit", "pose": {"id": "wave-right", "mirror": true}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct Preview {
    /// The character, built from the kit or imported.
    pub character: CharId,
    /// A variant of a kit character to render instead of its base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
    /// The scene to stand it in, alone, in its subject's place; the project's opening scene when left out (the built-in
    /// studio scene in a project without one). A library item previews by being applied to a scratch default scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// A camera of the scene; the one it opens with when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraId>,
    /// The shot: `face`, `bust`, `full` or `hands`. With `camera` it replaces that camera's framing; without, the
    /// picture is a fresh camera of this framing from the front.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<Framing>,
    /// Degrees around the character: 0 is in front, + toward screen right. `-180..=180`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    /// Degrees: + looks from above. `-90..=90`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    /// Above 1 moves in from the preset's distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f32>,
    /// A light rig (`stage.light_rigs` lists them) in place of the scene's own lighting, for this picture only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_rig: Option<String>,
    /// An expression of a kit character's face profile to show; the neutral face when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<ExpressionId>,
    /// The expression's weight, 0 to 1.
    #[serde(default = "one")]
    pub weight: f32,
    /// A pose of the pose library (`pose.list`) to show, mirrored or blended with another; the character as sculpted
    /// when left out. The shot is framed on the character as sculpted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<PoseArg>,
    /// Width and height in pixels, each 16 to 4096.
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// `live`, `high` or `offline` (the default, the same bytes on every run).
    #[serde(default)]
    pub quality: RenderQuality,
    /// A named render preset (`measure`) that fixes part of the picture; the arguments it fixes are left out.
    /// `measure`: the face framing through an orthographic camera from the front, the `measure-flat` rig, the neutral
    /// face, no post effects, and every line `#1a1a1a` and at least 1.5 px wide; it asks for at least 160 px between
    /// the eyes (a 768 px square gives about 250).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
}

/// What a preview gives back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewResult {
    /// `omuzaki://render/<hash>.png`.
    pub uri: String,
    /// The PNG's content hash in the asset store.
    pub hash: String,
    /// Width and height in pixels.
    pub size: [u32; 2],
    /// `kit` for a character built from parts, `imported` for an imported avatar.
    pub body: String,
    /// The scene it stood in; `None` for the built-in default of a project without scenes.
    pub scene: Option<String>,
    /// The scene camera it was shot from (`shot` for a fresh camera of the framing asked for).
    pub camera: String,
    /// For each character whose painted face is in the picture and turned toward the camera, where its face layout's
    /// eye centres, mouth and chin landed; empty when none is (an imported avatar has no painted face).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub face_landmarks: Vec<RenderedFace>,
}

/// Where one character's face landed in the picture: pixels, origin top-left, x right, y down, pixel centres at .5
/// (likeness-measures §5), ready to pass to a `measure.*` op.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RenderedFace {
    /// The character.
    pub character: String,
    /// The centre of the character's right eye (on the picture's left in a front view).
    pub eye_right: [f64; 2],
    /// The centre of the character's left eye.
    pub eye_left: [f64; 2],
    /// The centre of the mouth line.
    pub mouth: [f64; 2],
    /// The bottom of the head outline.
    pub chin: [f64; 2],
    /// Who placed the points: always `render`.
    pub source: String,
}

impl RenderedFace {
    fn new(character: &CharId, points: &FacePoints) -> Self {
        // To a hundredth of a pixel: finer than any measure reads, and the JSON stays short.
        let at = |p: omuzaki_core::glam::Vec2| [p.x, p.y].map(|v| (f64::from(v) * 100.0).round() / 100.0);
        Self {
            character: character.to_string(),
            eye_right: at(points.eye_right),
            eye_left: at(points.eye_left),
            mouth: at(points.mouth),
            chin: at(points.chin),
            source: "render".into(),
        }
    }
}

/// A scene rule broken by the arguments, as the op's refusal naming the field.
fn refusal(error: SceneError) -> OpError {
    let message = error.to_string();
    OpError::invalid(error.path, message)
}

/// The lighting of the library rig `name`.
fn rig_lighting(name: &str) -> Result<Lighting, OpError> {
    if !LightRig::names().any(|known| known == name) {
        let known: Vec<&str> = LightRig::names().collect();
        return Err(OpError::invalid("light_rig", format!("there is no light rig '{name}'"))
            .hint(format!("the light rigs are {}", known.join(", "))));
    }
    LightRig::load(name)
        .and_then(|rig| rig.to_lighting())
        .map_err(|e| OpError::internal(format!("the light rig '{name}' doesn't load: {e}")))
}

/// The library preset `name`, or the refusal naming `preset` with the presets there are.
fn load_preset(name: &str) -> Result<RenderPreset, OpError> {
    if !RenderPreset::names().any(|known| known == name) {
        let known: Vec<&str> = RenderPreset::names().collect();
        return Err(OpError::invalid("preset", format!("there is no render preset '{name}'"))
            .hint(format!("the render presets are {}", known.join(", "))));
    }
    RenderPreset::load(name).map_err(|e| OpError::internal(format!("the render preset '{name}' doesn't load: {e}")))
}

impl Preview {
    /// The preset asked for, loaded, with the arguments it fixes refused when given too.
    fn preset(&self) -> Result<Option<RenderPreset>, OpError> {
        let Some(name) = &self.preset else { return Ok(None) };
        let preset = load_preset(name)?;
        let camera = preset.camera.is_some();
        let fixed = [
            ("camera", camera && self.camera.is_some()),
            ("framing", camera && self.framing.is_some()),
            ("yaw", camera && self.yaw.is_some()),
            ("pitch", camera && self.pitch.is_some()),
            ("zoom", camera && self.zoom.is_some()),
            ("light_rig", preset.light_rig.is_some() && self.light_rig.is_some()),
            ("expression", preset.expression.is_some() && self.expression.is_some()),
        ];
        if let Some((field, _)) = fixed.into_iter().find(|(_, both)| *both) {
            return Err(OpError::invalid(field, format!("the '{name}' preset sets `{field}` itself"))
                .hint(format!("leave out `{field}`, or the preset")));
        }
        Ok(Some(preset))
    }

    fn choice(&self, preset: Option<&RenderPreset>) -> ShotChoice {
        let mut choice = ShotChoice::default();
        if let Some(camera) = preset.and_then(|p| p.camera) {
            choice.framing = Some(camera.framing);
            choice.yaw = Some(camera.yaw);
            choice.pitch = Some(camera.pitch);
            return choice;
        }
        choice.camera.clone_from(&self.camera);
        choice.framing = self.framing;
        choice.yaw = self.yaw;
        choice.pitch = self.pitch;
        choice.zoom = self.zoom;
        choice
    }

    fn check(&self) -> Result<Option<RenderPreset>, OpError> {
        check_size(self.size, self.quality)?;
        if !(0.0..=1.0).contains(&self.weight) {
            return Err(OpError::invalid("weight", format!("weight {} is outside 0 to 1", self.weight)));
        }
        if let Some(pose) = &self.pose {
            pose.check("pose")?;
        }
        self.preset()
    }

    /// The expression the picture shows: the preset's when it fixes one, else the one asked for.
    fn expression(&self, preset: Option<&RenderPreset>) -> Result<Option<ExpressionId>, OpError> {
        match preset.map(RenderPreset::expression_id).transpose() {
            Ok(Some(Some(fixed))) => Ok(fixed),
            Ok(_) => Ok(self.expression.clone()),
            Err(e) => Err(OpError::internal(e.to_string())),
        }
    }

    /// The scene the picture is taken in: a copy of the scene with the character alone in it, the light rig and the
    /// shot applied, checked. Reads the document only.
    fn staged(
        &self,
        doc: &dyn DocRead,
        preset: Option<&RenderPreset>,
    ) -> Result<(Option<SceneId>, SceneDef, MemberId, CameraId), OpError> {
        let (id, mut def) = scene_for(doc, self.scene.as_ref())?;
        if let Some(rig) = preset.and_then(|p| p.light_rig.as_ref()).or(self.light_rig.as_ref()) {
            def.lighting = rig_lighting(rig)?;
        }
        let member = cast_alone(&mut def, &self.character, self.variant.as_ref())
            .map_err(|e| OpError::internal(format!("casting the character: {e}")))?;
        let camera = self.choice(preset).apply(&mut def).map_err(|e| {
            let field = if e.path == "camera" { "camera" } else { "framing" };
            OpError::invalid(field, e.rule.clone())
                .hint("stage.describe_scene lists the scene's cameras; framing takes face, bust, full or hands")
        })?;
        def.check().map_err(refusal)?;
        Ok((id, def, member, camera))
    }
}

impl Op for Preview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.preview",
            summary: "Render one picture of a character (built or imported) standing alone in a scene (the project's \
                      opening scene by default, or `scene`), from one of its cameras or a preset framing (face, bust, \
                      full, hands), optionally under another light rig, and store it as a PNG; returns its \
                      omuzaki://render/ URI and where the face's eyes, mouth and chin landed (`face_landmarks`, for \
                      the measure.* ops). `preset: \"measure\"` gives the measurement look: an orthographic front face \
                      shot, flat light, the neutral face, no post effects and a dark line. The document never changes.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // Its finish plans no change (the PNG goes to the content store), so its empty patch is undone exactly and
            // nothing enters the history.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let preset = self.check()?;
        if ctx.doc.get(&omuzaki_char_api::paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        self.staged(ctx.doc, preset.as_ref())?;
        let size = check_size(self.size, self.quality)?;
        let character = character_bytes(ctx, &self.character, self.variant.as_ref(), self.quality)?;
        Ok(Plan { cost: Some(cost(size, 1, self.quality, character)), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let size = check_size(self.size, self.quality)?;
        let preset = self.check()?;
        let preset = preset.as_ref();
        let clock = SystemClock::default();
        let build = job.service::<BuildServices>()?;
        let services = BodyServices::new(
            job.service::<dyn CharacterBuilder>()?,
            build,
            job.service::<dyn Store>()?,
            job.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon),
            &clock,
        );
        job.checkpoint("loading", 0.0)?;
        let (scene, def, member, camera_id) = job.read(|doc| self.staged(doc, preset))?;
        let mut body =
            job.read(|doc| load_body(doc, &services, &self.character, self.variant.as_ref(), self.quality.detail()))?;
        let kind = match &body {
            Body::Kit(_) => "kit",
            Body::Avatar(_) => "imported",
        };
        let landmarks = body.landmarks().map_err(|e| OpError::invalid("framing", e.to_string()))?;
        #[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
        let aspect = size.x as f32 / size.y as f32;
        let mut staged =
            stage_alone(&def, &member, landmarks, aspect).map_err(|e| OpError::invalid("scene", e.to_string()))?;
        if staged.resolved.environment.and_then(|e| e.hdri()).is_some() {
            staged.resolved.load_hdri(job.service::<dyn Store>()?).map_err(|e| {
                OpError::invalid("scene", e.to_string())
                    .hint("an HDRI environment names a Radiance .hdr in the asset store")
            })?;
        }
        if preset.is_some_and(|p| p.post == PresetPost::Off) {
            staged.resolved.post =
                Post { bloom: None, depth_of_field: None, grain: None, grade: None, ..staged.resolved.post };
        } else if let Body::Kit(built) = &body {
            // A kit character's look sets part of the frame too: its bloom, depth of field, grain and grade.
            post_in_look(&mut staged.resolved.post, &built.look().scene);
        }
        if preset.and_then(|p| p.camera).is_some_and(|c| c.projection == PresetProjection::Orthographic) {
            orthographic(&mut staged, &landmarks)?;
        }
        let camera = staged.camera().ok_or_else(|| OpError::internal("the staged scene has no camera"))?;
        if let Some(line) = preset.and_then(|p| p.outline) {
            force_line(&mut body, line, &camera, head_reach(&staged, &landmarks), size.y);
        }
        let mut shot = Shot::new(camera);
        if let Some(expression) = self.expression(preset)? {
            check_expression(&body, &expression)?;
            shot = shot.with_expression(expression, self.weight);
        }
        job.checkpoint("rendering", 0.2)?;
        let pose = self.pose.as_ref().map(|pose| pose.resolve("pose", &body)).transpose()?;
        let spec = StillSpec::in_scene(staged, size, self.quality.quality()).with_pose(pose);
        let stills = with_renderer(job, |renderer| render_body_stills(renderer, body, &spec, &[shot]))?;
        let still = stills.into_iter().next().ok_or_else(|| OpError::internal("the render gave no picture"))?;
        if let (Some(min), Some(face)) = (preset.and_then(|p| p.min_iod_px), &still.face)
            && face.iod() < min
        {
            let name = self.preset.as_deref().unwrap_or_default();
            return Err(OpError::invalid(
                "size",
                format!(
                    "the eyes are {:.0} px apart in the picture; the '{name}' preset needs at least {min}",
                    face.iod()
                ),
            )
            .hint("give a larger size: a 768 px square gives about 250 px between the eyes"));
        }
        let png = encode_png(&still.image).map_err(|e| OpError::internal(e.to_string()))?;
        let stored = store_png(job, &png)?;
        job.checkpoint("done", 1.0)?;
        let result = PreviewResult {
            uri: stored.uri,
            hash: stored.hash.to_string(),
            size: self.size,
            body: kind.into(),
            scene: scene.map(|s| s.to_string()),
            camera: camera_id.to_string(),
            face_landmarks: still.face.iter().map(|face| RenderedFace::new(&self.character, face)).collect(),
        };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the preview: {e}")))
    }
}

/// Where the head is in the world and how far it reaches from its centre: the centre of its bind bounds placed where
/// the subject stands, and half their diagonal. The whole body's when it has no head part.
fn head_reach(staged: &StagedScene, landmarks: &Landmarks) -> (Vec3, f32) {
    let bounds = landmarks.head.unwrap_or(landmarks.body);
    let centre = staged.subject_root.transform_point3(bounds.centre());
    (centre, (bounds.max - bounds.min).length() * 0.5)
}

/// Makes the staged scene's active camera orthographic, framing at the head what its lens framed there: the view height
/// is the lens's frame height at the head's depth, so the face fills the picture as it did, without the lens's
/// perspective between the eyes and the chin.
fn orthographic(staged: &mut StagedScene, landmarks: &Landmarks) -> Result<(), OpError> {
    let (head, _) = head_reach(staged, landmarks);
    let active = staged.resolved.active;
    let (_, camera) = active
        .and_then(|i| staged.resolved.cameras.get_mut(i))
        .ok_or_else(|| OpError::internal("the staged scene has no camera"))?;
    if let Projection::Perspective { fov_y_deg, near, far } = camera.projection {
        let depth = (camera.position() - head).dot(camera.transform.z_axis.truncate());
        if depth <= near {
            return Err(OpError::internal("the preset's camera is not in front of the head"));
        }
        let height = 2.0 * depth * (fov_y_deg.to_radians() * 0.5).tan();
        camera.projection = Projection::Orthographic { height, near, far };
    }
    Ok(())
}

/// Gives every outlined material of `body` the preset's line: its colour, and a width that draws at least `min_px`
/// pixels on the head in a picture `height` pixels tall from `camera`. A wider line keeps its width. The line's width
/// grows with depth ([`Outline::world_width`]), so the width is set for the head's nearest point, `head` being the
/// head's centre and how far it reaches. An imported avatar's own `MToon` line styles give way to the kit's line, so the
/// width means the same on every material.
fn force_line(body: &mut Body, line: PresetOutline, camera: &Camera, (head, reach): (Vec3, f32), height: u32) {
    let depth = (camera.position() - head).dot(camera.transform.z_axis.truncate());
    let frame_height = match camera.projection {
        Projection::Perspective { fov_y_deg, .. } => 2.0 * depth * (fov_y_deg.to_radians() * 0.5).tan(),
        Projection::Orthographic { height, .. } => height,
    };
    #[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
    let metres_per_px = frame_height / height as f32;
    let unit = Outline { colour: line.colour, width: 1.0 };
    let width = line.min_px * metres_per_px / unit.world_width(depth - reach);
    let materials = match body {
        Body::Kit(built) => &mut built.materials,
        Body::Avatar(avatar) => {
            avatar.outlines.clear();
            &mut avatar.materials
        }
    };
    for outline in materials.values_mut().filter_map(|m| m.outline.as_mut()) {
        outline.colour = line.colour;
        outline.width = outline.width.max(width);
    }
}

register_op!(Preview);
