//! `render.preview`: one picture of a character in a scene, stored as a PNG (spec v3 §11, design scenes D9). It is how
//! an agent sees what it made: any character of the project, built from the kit or imported, renders through the same
//! call, so a person and an agent look at the same pixels. The character stands alone in a copy of the scene (the
//! project's opening scene, or the built-in default), lit, framed and backed as that scene is; the document never
//! changes.

use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, VariantId};
use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::DocRead;
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Capabilities, MtoonSupport};
use omuzaki_stage_api::{CameraId, Framing, Lighting, MemberId, SceneDef, SceneError, SceneId};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::job::{RenderQuality, check_size, cost, store_png, with_renderer};
use super::still::{Shot, StillSpec, encode_png, render_body_stills};
use crate::body::{BodyServices, load_body};
use crate::recipe::LightRig;
use crate::scene::alone::{ShotChoice, cast_alone, stage_alone};
use crate::scene::default::scene_for;

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
        {"character": "kit", "light_rig": "viewer-candle", "framing": "face"}
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
    /// Width and height in pixels, each 16 to 4096.
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// `live`, `high` or `offline` (the default, the same bytes on every run).
    #[serde(default)]
    pub quality: RenderQuality,
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

impl Preview {
    fn choice(&self) -> ShotChoice {
        let mut choice = ShotChoice::default();
        choice.camera.clone_from(&self.camera);
        choice.framing = self.framing;
        choice.yaw = self.yaw;
        choice.pitch = self.pitch;
        choice.zoom = self.zoom;
        choice
    }

    fn check(&self) -> Result<(), OpError> {
        check_size(self.size)?;
        if !(0.0..=1.0).contains(&self.weight) {
            return Err(OpError::invalid("weight", format!("weight {} is outside 0 to 1", self.weight)));
        }
        Ok(())
    }

    /// The scene the picture is taken in: a copy of the scene with the character alone in it, the light rig and the
    /// shot applied, checked. Reads the document only.
    fn staged(&self, doc: &dyn DocRead) -> Result<(Option<SceneId>, SceneDef, MemberId, CameraId), OpError> {
        let (id, mut def) = scene_for(doc, self.scene.as_ref())?;
        if let Some(rig) = &self.light_rig {
            def.lighting = rig_lighting(rig)?;
        }
        let member = cast_alone(&mut def, &self.character, self.variant.as_ref())
            .map_err(|e| OpError::internal(format!("casting the character: {e}")))?;
        let camera = self.choice().apply(&mut def).map_err(|e| {
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
                      omuzaki://render/ URI. The document never changes.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // Its finish plans no change (the PNG goes to the content store), so its empty patch is undone exactly and
            // nothing enters the history.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.check()?;
        if ctx.doc.get(&omuzaki_char_api::paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        self.staged(ctx.doc)?;
        let size = check_size(self.size)?;
        Ok(Plan { cost: Some(cost(size, 1, self.quality)), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let size = check_size(self.size)?;
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
        let (scene, def, member, camera_id) = job.read(|doc| self.staged(doc))?;
        let body = job.read(|doc| load_body(doc, &services, &self.character, self.variant.as_ref(), self.quality.detail()))?;
        let kind = match &body {
            crate::body::Body::Kit(_) => "kit",
            crate::body::Body::Avatar(_) => "imported",
        };
        let landmarks = body.landmarks().map_err(|e| OpError::invalid("framing", e.to_string()))?;
        #[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
        let aspect = size.x as f32 / size.y as f32;
        let staged = stage_alone(&def, &member, landmarks, aspect).map_err(|e| OpError::invalid("scene", e.to_string()))?;
        let camera = staged.camera().ok_or_else(|| OpError::internal("the staged scene has no camera"))?;
        let mut shot = Shot::new(camera);
        if let Some(expression) = &self.expression {
            shot = shot.with_expression(expression.clone(), self.weight);
        }
        job.checkpoint("rendering", 0.2)?;
        let spec = StillSpec::in_scene(staged, size, self.quality.quality());
        let stills = with_renderer(job, |renderer| render_body_stills(renderer, body, &spec, &[shot]))?;
        let still = stills.into_iter().next().ok_or_else(|| OpError::internal("the render gave no picture"))?;
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
        };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the preview: {e}")))
    }
}

register_op!(Preview);
