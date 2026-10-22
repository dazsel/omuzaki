//! `render.preview`: one picture of a character, framed by a preset and lit by a light rig, stored as a PNG (spec v3
//! §11). It is how an agent sees what it made: any character of the project, built from the kit or imported, renders
//! through the same call, so a person and an agent look at the same pixels.

use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, VariantId};
use omuzaki_core::time::SystemClock;
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Capabilities, MtoonSupport};
use omuzaki_stage_api::{CameraDef, Framing};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::job::{DEFAULT_LIGHT_RIG, RenderQuality, check_size, cost, still_light, store_png, with_renderer};
use super::still::{Shot, StillSpec, encode_png, render_body_stills};
use crate::body::{BodyServices, load_body};
use crate::scene::frame::frame;

/// The size of a preview when none is given: a portrait frame.
const DEFAULT_SIZE: [u32; 2] = [768, 1024];

fn default_size() -> [u32; 2] {
    DEFAULT_SIZE
}

fn default_light_rig() -> String {
    DEFAULT_LIGHT_RIG.to_owned()
}

fn one() -> f32 {
    1.0
}

/// Renders one picture of a character and stores it as a PNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "framing": "bust", "yaw": 30.0, "size": [512, 512], "quality": "live"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct Preview {
    /// The character, built from the kit or imported.
    pub character: CharId,
    /// A variant of a kit character to render instead of its base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
    /// The shot: `face`, `bust`, `full` (the default) or `hands`.
    #[serde(default)]
    pub framing: Framing,
    /// Degrees around the character: 0 is in front, + toward screen right. `-180..=180`.
    #[serde(default)]
    pub yaw: f32,
    /// Degrees: + looks from above. `-90..=90`.
    #[serde(default)]
    pub pitch: f32,
    /// Above 1 moves in from the preset's distance.
    #[serde(default = "one")]
    pub zoom: f32,
    /// The light rig, by name (`viewer-studio` by default).
    #[serde(default = "default_light_rig")]
    pub light_rig: String,
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
}

impl Preview {
    /// The camera definition the arguments describe.
    fn camera(&self) -> CameraDef {
        let mut camera = CameraDef::preset(self.framing);
        camera.yaw = self.yaw;
        camera.pitch = self.pitch;
        camera.zoom = self.zoom;
        camera
    }

    fn check(&self) -> Result<(), OpError> {
        check_size(self.size)?;
        if self.framing == Framing::Free {
            return Err(OpError::invalid("framing", "a preview frames by a preset")
                .hint("use face, bust, full or hands; yaw, pitch and zoom move the camera"));
        }
        if !(0.0..=1.0).contains(&self.weight) {
            return Err(OpError::invalid("weight", format!("weight {} is outside 0 to 1", self.weight)));
        }
        still_light(&self.light_rig).map(drop)
    }
}

impl Op for Preview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.preview",
            summary: "Render one picture of a character (built or imported), framed by a preset (face, bust, full, \
                      hands) and lit by a light rig, and store it as a PNG; returns its omuzaki://render/ URI.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.check()?;
        if ctx.doc.get(&omuzaki_char_api::paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        let size = check_size(self.size)?;
        Ok(Plan { cost: Some(cost(size, 1, self.quality)), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let size = check_size(self.size)?;
        let light = still_light(&self.light_rig)?;
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
        let body = job.read(|doc| load_body(doc, &services, &self.character, self.variant.as_ref(), self.quality.detail()))?;
        let kind = match &body {
            crate::body::Body::Kit(_) => "kit",
            crate::body::Body::Avatar(_) => "imported",
        };
        let landmarks = body.landmarks().map_err(|e| OpError::invalid("framing", e.to_string()))?;
        #[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
        let aspect = size.x as f32 / size.y as f32;
        let camera = frame(self.framing, &landmarks, &self.camera(), aspect)
            .map_err(|e| OpError::invalid("framing", e.to_string()))?;
        let mut shot = Shot::new(camera);
        if let Some(expression) = &self.expression {
            shot = shot.with_expression(expression.clone(), self.weight);
        }
        job.checkpoint("rendering", 0.2)?;
        let spec = StillSpec::new(light, size, self.quality.quality());
        let stills = with_renderer(job, |renderer| render_body_stills(renderer, body, &spec, &[shot]))?;
        let still = stills.into_iter().next().ok_or_else(|| OpError::internal("the render gave no picture"))?;
        let png = encode_png(&still.image).map_err(|e| OpError::internal(e.to_string()))?;
        let stored = store_png(job, &png)?;
        job.checkpoint("done", 1.0)?;
        let result = PreviewResult { uri: stored.uri, hash: stored.hash.to_string(), size: self.size, body: kind.into() };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the preview: {e}")))
    }
}

register_op!(Preview);
